//! Notes provider settings and write endpoints.
//!
//! This API is intentionally separate from task/internal-task storage. Changing
//! a notes provider or notes root changes where human-readable notes are saved;
//! it does not move or mutate canonical task, execution, ledger, or artifact
//! runtime state.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use actix_files::NamedFile;
use actix_multipart::Multipart;
use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::fs;

use crate::chat_api::{open_folder_in_file_manager, validate_browser_origin};
use crate::media_api::read_uploaded_audio_form;
use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifact_v2::{
    service::{ArtifactV2Service, ScopeRef, V3ReadApi},
    ArtifactV2Error,
};
use magician::magician_v2::learning::{
    LearningCandidateState, LearningMemoryBridge, LearningScope, LearningStore,
};
use magician::magician_v2::notes::{
    AppendNoteRequest, CaptureSelectionRequest, CreateNoteRequest, NoteSearchRequest,
    NotesSettings, NotesSettingsStore, PublishTaskNoteRequest, SaveAudioNoteRequest,
    TaskNotePublishMode,
};
// The notes projection seam (plan 2.3) owns the memory-promotion projection
// behind promote-to-memory; this handler keeps only transport and review
// routing.
use magician::magician_v2::notes_projection::{
    bounded_note_memory_value, build_task_note_memory_candidate_request,
    task_note_memory_candidate_id,
};
// Only this module's tests name these directly; the request and note shapes
// are built behind the projection seam now.
#[cfg(test)]
use magician::magician_v2::learning::LearningCandidateType;
#[cfg(test)]
use magician::magician_v2::notes::TaskNoteIndexEntry;

#[derive(Clone)]
pub struct NotesApi {
    settings: Arc<NotesSettingsStore>,
    task_service: Arc<OnceLock<Arc<ArtifactV2Service>>>,
    learning_store: LearningStore,
    memory_bridge: LearningMemoryBridge,
}

impl NotesApi {
    pub fn new<P: AsRef<std::path::Path>>(storage_root: P) -> Self {
        Self::with_workspace_layout(ArtifactV2Workspace::new(storage_root))
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        let learning_store = LearningStore::new(workspace_layout.clone());
        let memory_bridge = LearningMemoryBridge::new(workspace_layout.clone());
        let settings = Arc::new(NotesSettingsStore::with_workspace_layout(workspace_layout));
        #[cfg(not(test))]
        settings.spawn_notes_watches();
        Self {
            settings,
            task_service: Arc::new(OnceLock::new()),
            learning_store,
            memory_bridge,
        }
    }

    pub fn set_task_service(&self, service: Arc<ArtifactV2Service>) {
        let _ = self.task_service.set(service);
    }

    pub fn settings_store(&self) -> Arc<NotesSettingsStore> {
        Arc::clone(&self.settings)
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct NotesScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AudioNotesListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TaskNotesListQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub q: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ScopedPublishTaskNoteRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub publish: PublishTaskNoteRequest,
}

#[derive(Debug, Deserialize, Default)]
pub struct BackfillTaskNotesRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub mode: Option<TaskNotePublishMode>,
    #[serde(default)]
    pub include_assets: Option<bool>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default = "default_true_value")]
    pub only_unpublished: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct PromoteTaskNoteMemoryRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

fn default_true_value() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct PutNotesSettingsRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub settings: NotesSettings,
}

#[derive(Debug, Deserialize, Default)]
pub struct OpenNotesProviderRootRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenNotesProviderRootResponse {
    pub provider: String,
    pub root_path: String,
}

pub async fn get_notes_settings_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    match api.settings.load_envelope(&principal, &workspace).await {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn put_notes_settings_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<PutNotesSettingsRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .save(&principal, &workspace, body.settings)
        .await
    {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn notes_provider_status_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    match api.settings.provider_status(&principal, &workspace).await {
        Ok(status) => Ok(HttpResponse::Ok().json(status)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn open_notes_provider_root_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: Option<web::Json<OpenNotesProviderRootRequest>>,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }

    let body = body.map(web::Json::into_inner).unwrap_or_default();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let envelope = match api.settings.load_envelope(&principal, &workspace).await {
        Ok(envelope) => envelope,
        Err(error) => return Ok(notes_io_error_response(error)),
    };
    let requested_provider = body
        .provider
        .as_deref()
        .unwrap_or(envelope.resolved.default_provider.as_str());
    let Some(provider) = normalize_open_root_provider(requested_provider) else {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "unknown_notes_provider",
            "message": format!("Unknown notes provider: {requested_provider}"),
        })));
    };
    let root_path = match provider {
        "silverbullet" => envelope.resolved.silverbullet_space_path,
        _ => envelope.resolved.local_markdown_root,
    };
    if root_path.trim().is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "notes_provider_root_unconfigured",
            "message": format!("Notes provider `{provider}` has no root path configured"),
        })));
    }
    let root = PathBuf::from(&root_path);
    if let Err(error) = fs::create_dir_all(&root).await {
        return Ok(notes_io_error_response(error));
    }
    match open_folder_in_file_manager(&root).await {
        Ok(()) => Ok(HttpResponse::Ok().json(OpenNotesProviderRootResponse {
            provider: provider.to_string(),
            root_path,
        })),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "notes_provider_open_root_failed",
            "message": error.to_string(),
        }))),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScopedCreateNoteRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub note: CreateNoteRequest,
}

pub async fn create_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<ScopedCreateNoteRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .create_note(&principal, &workspace, body.note)
        .await
    {
        Ok(note_ref) => Ok(HttpResponse::Created().json(note_ref)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScopedCaptureSelectionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub capture: CaptureSelectionRequest,
}

/// File a selection from wherever the owner made it.
///
/// Separate from `append` because a capture carries provenance and a retry
/// contract that a plain append does not: `capture_id` makes a resend after a
/// lost response land once, which matters when the caller is a browser
/// extension on a flaky localhost connection rather than a person.
pub async fn capture_selection_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<ScopedCaptureSelectionRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .capture_selection(&principal, &workspace, body.capture)
        .await
    {
        Ok(note_ref) => Ok(HttpResponse::Ok().json(note_ref)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScopedNoteSearchRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub search: NoteSearchRequest,
}

#[derive(Debug, Deserialize)]
pub struct NotePathQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub path: String,
}

pub async fn list_note_tree_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotePathQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .list_note_tree(&principal, &workspace, &query.path)
        .await
    {
        Ok(tree) => Ok(HttpResponse::Ok().json(tree)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn list_note_backlinks_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotePathQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .list_note_backlinks(&principal, &workspace, &query.path)
        .await
    {
        Ok(backlinks) => Ok(HttpResponse::Ok().json(backlinks)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateMarkdownNoteRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub folder: String,
    pub name: String,
}

pub async fn create_markdown_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<CreateMarkdownNoteRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .create_markdown_note(&principal, &workspace, &body.folder, &body.name)
        .await
    {
        Ok(file) => Ok(HttpResponse::Ok().json(file)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct SaveNoteRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub path: String,
    pub markdown: String,
}

pub async fn save_note_markdown_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<SaveNoteRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .save_note_markdown(&principal, &workspace, &body.path, &body.markdown)
        .await
    {
        Ok(file) => Ok(HttpResponse::Ok().json(file)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn create_note_folder_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<CreateMarkdownNoteRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .create_note_folder(&principal, &workspace, &body.folder, &body.name)
        .await
    {
        Ok(relative_path) => Ok(HttpResponse::Ok().json(json!({
            "relative_path": relative_path,
            "kind": "dir",
        }))),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn delete_markdown_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotePathQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .delete_markdown_note(&principal, &workspace, &query.path)
        .await
    {
        Ok(()) => Ok(HttpResponse::NoContent().finish()),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn delete_note_folder_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotePathQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .delete_note_folder(&principal, &workspace, &query.path)
        .await
    {
        Ok(()) => Ok(HttpResponse::NoContent().finish()),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn read_note_file_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotePathQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .read_note_file(&principal, &workspace, &query.path)
        .await
    {
        Ok(file) => Ok(HttpResponse::Ok().json(file)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn search_notes_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<ScopedNoteSearchRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .search_notes(&principal, &workspace, body.search)
        .await
    {
        Ok(results) => Ok(HttpResponse::Ok().json(results)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScopedAppendNoteRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(flatten)]
    pub note: AppendNoteRequest,
}

pub async fn append_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<ScopedAppendNoteRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .append_note(&principal, &workspace, body.note)
        .await
    {
        Ok(note_ref) => Ok(HttpResponse::Ok().json(note_ref)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

/// Persist a voice recording and its transcript as a dated Audio Notes page.
///
/// The multipart body accepts one `file`/`audio` part plus optional
/// `note_id`, `captured_at`, `transcript`, `source_surface`, `duration_ms`, and
/// `provider` fields. A stable UUID `note_id` makes outbox retries idempotent.
/// When `provider` is omitted, the scope's configured default and fallback are
/// used exactly like every other Notes write.
pub async fn create_audio_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
    payload: Multipart,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }
    let query = query.into_inner();
    let form = match read_uploaded_audio_form(payload).await {
        Ok(form) => form,
        Err(response) => return Ok(response),
    };
    let workspace_field = form.fields.get("workspace").cloned();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.or(workspace_field)) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    let duration_ms = match form
        .fields
        .get("duration_ms")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse::<u64>)
        .transpose()
    {
        Ok(value) => value,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "invalid_audio_note_duration",
                "message": "duration_ms must be a non-negative integer",
            })))
        },
    };
    let provider = form
        .fields
        .get("provider")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let source_surface = form
        .fields
        .get("source_surface")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("voice_note")
        .to_string();
    let request = SaveAudioNoteRequest {
        provider,
        note_id: form.fields.get("note_id").cloned(),
        captured_at: form.fields.get("captured_at").cloned(),
        source_surface,
        transcript: form.fields.get("transcript").cloned(),
        original_filename: form.filename,
        mime_type: form.mime_type,
        duration_ms,
        bytes: form.bytes,
    };

    match api
        .settings
        .save_audio_note(&principal, &workspace, request)
        .await
    {
        Ok(note_ref) => Ok(HttpResponse::Created().json(note_ref)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn list_audio_notes_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<AudioNotesListQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .list_audio_notes(
            &principal,
            &workspace,
            query.offset.unwrap_or_default(),
            query.limit.unwrap_or(20),
            query.q.as_deref(),
        )
        .await
    {
        Ok(page) => Ok(HttpResponse::Ok().json(page)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn get_audio_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
    note_id: web::Path<String>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    match api
        .settings
        .read_audio_note(&principal, &workspace, note_id.as_str())
        .await
    {
        Ok(Some(note)) => Ok(HttpResponse::Ok().json(note)),
        Ok(None) => Ok(HttpResponse::NotFound().json(json!({
            "error": "audio_note_not_found",
        }))),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn get_audio_note_recording_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
    note_id: web::Path<String>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    let recording = match api
        .settings
        .audio_note_recording(&principal, &workspace, note_id.as_str())
        .await
    {
        Ok(Some(recording)) => recording,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "audio_note_recording_not_found",
            })))
        },
        Err(error) => return Ok(notes_io_error_response(error)),
    };
    match NamedFile::open_async(&recording.absolute_path).await {
        Ok(file) => {
            let mut response = file.into_response(&req);
            response.headers_mut().insert(
                actix_web::http::header::CACHE_CONTROL,
                actix_web::http::header::HeaderValue::from_static("private, no-store"),
            );
            response.headers_mut().insert(
                actix_web::http::header::HeaderName::from_static("x-content-type-options"),
                actix_web::http::header::HeaderValue::from_static("nosniff"),
            );
            Ok(response)
        },
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn delete_audio_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
    note_id: web::Path<String>,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    match api
        .settings
        .delete_audio_note(&principal, &workspace, note_id.as_str())
        .await
    {
        Ok(true) => Ok(HttpResponse::NoContent().finish()),
        Ok(false) => Ok(HttpResponse::NotFound().json(json!({
            "error": "audio_note_not_found",
        }))),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn publish_task_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    task_id: web::Path<String>,
    body: Option<web::Json<ScopedPublishTaskNoteRequest>>,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }
    let body = body.map(web::Json::into_inner).unwrap_or_default();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(service) = api.task_service.get() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "task_notes_service_unavailable",
        })));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
    let task_id = task_id.into_inner();
    let task = match service.get_task(&scope, &task_id).await {
        Ok(task) => task,
        Err(error) => return Ok(task_artifact_error_response(error)),
    };
    match api
        .settings
        .publish_task_note(&principal, &workspace, &task, body.publish)
        .await
    {
        Ok(note) => Ok(HttpResponse::Ok().json(note)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn backfill_task_notes_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    body: web::Json<BackfillTaskNotesRequest>,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(service) = api.task_service.get() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "task_notes_service_unavailable",
        })));
    };
    let scope = ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
    let completed_tasks = match service.list_tasks(&scope).await {
        Ok(tasks) => tasks
            .into_iter()
            .filter(|task| task.status == "completed")
            .collect::<Vec<_>>(),
        Err(error) => return Ok(task_artifact_error_response(error)),
    };
    let completed_total = completed_tasks.len();
    let mut tasks = Vec::with_capacity(completed_total);
    if body.only_unpublished {
        for task in completed_tasks {
            match api
                .settings
                .task_note_is_available(&principal, &workspace, &task.id)
                .await
            {
                Ok(true) => {},
                Ok(false) => tasks.push(task),
                Err(error) => {
                    return Ok(notes_io_error_response(error));
                },
            }
        }
    } else {
        tasks = completed_tasks;
    }
    let total = tasks.len();
    let offset = body.offset.unwrap_or_default().min(total);
    let limit = body.limit.unwrap_or(25).clamp(1, 100);
    let selected = tasks
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let mut published = Vec::new();
    let skipped: Vec<String> = Vec::new();
    let mut errors = Vec::new();
    for item in selected {
        let task = match service.get_task(&scope, &item.id).await {
            Ok(task) => task,
            Err(error) => {
                errors.push(json!({ "task_id": item.id, "error": error.to_string() }));
                continue;
            },
        };
        match api
            .settings
            .publish_task_note(
                &principal,
                &workspace,
                &task,
                PublishTaskNoteRequest {
                    provider: body.provider.clone(),
                    mode: body.mode,
                    include_assets: body.include_assets,
                },
            )
            .await
        {
            Ok(note) => published.push(note),
            Err(error) => errors.push(json!({
                "task_id": item.id,
                "error": error.to_string(),
            })),
        }
    }
    let consumed = published.len() + errors.len();
    let next_offset = if body.only_unpublished {
        0
    } else {
        offset.saturating_add(consumed)
    };
    let has_more = consumed < total.saturating_sub(offset);
    Ok(HttpResponse::Ok().json(json!({
        "published": published,
        "skipped_task_ids": skipped,
        "errors": errors,
        "pagination": {
            "total": total,
            "completed_total": completed_total,
            "offset": offset,
            "limit": limit,
            "has_more": has_more,
            "next_offset": has_more.then_some(next_offset),
        }
    })))
}

pub async fn list_task_notes_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<TaskNotesListQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .settings
        .list_task_notes(
            &principal,
            &workspace,
            query.offset.unwrap_or_default(),
            query.limit.unwrap_or(20),
            query.q.as_deref(),
        )
        .await
    {
        Ok(page) => Ok(HttpResponse::Ok().json(page)),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn get_task_note_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    query: web::Query<NotesScopeQuery>,
    task_id: web::Path<String>,
) -> Result<HttpResponse> {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    match api
        .settings
        .read_task_note(&principal, &workspace, task_id.as_str())
        .await
    {
        Ok(Some(note)) => Ok(HttpResponse::Ok().json(note)),
        Ok(None) => Ok(HttpResponse::NotFound().json(json!({
            "error": "task_note_not_found",
        }))),
        Err(error) => Ok(notes_io_error_response(error)),
    }
}

pub async fn promote_task_note_to_memory_handler(
    api: web::Data<Arc<NotesApi>>,
    req: HttpRequest,
    task_id: web::Path<String>,
    body: web::Json<PromoteTaskNoteMemoryRequest>,
) -> Result<HttpResponse> {
    if let Err(response) = validate_browser_origin(&req) {
        return Ok(response);
    }
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let task_id = task_id.into_inner();
    let (note, markdown) = match api
        .settings
        .read_task_note_markdown(&principal, &workspace, &task_id, 64 * 1024)
        .await
    {
        Ok(Some(note)) => note,
        Ok(None) => {
            return Ok(HttpResponse::NotFound().json(json!({
                "error": "task_note_not_found",
            })))
        },
        Err(error) => return Ok(notes_io_error_response(error)),
    };
    let memory_value = body
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(12_000).collect::<String>())
        .unwrap_or_else(|| bounded_note_memory_value(&markdown, 12_000));
    if memory_value.trim().is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "task_note_memory_value_empty",
        })));
    }
    let candidate_id =
        task_note_memory_candidate_id(&task_id, &note.source_updated_at, &memory_value);
    let scope = LearningScope::new(principal, workspace);
    let candidate = api.learning_store.ensure_candidate_with_id(
        scope.clone(),
        build_task_note_memory_candidate_request(&note, &task_id, memory_value),
        &candidate_id,
    );
    match candidate {
        Ok(candidate) => {
            if matches!(
                candidate.state,
                LearningCandidateState::Observed | LearningCandidateState::Proposed
            ) {
                if let Err(error) = api
                    .memory_bridge
                    .route_candidate(&api.learning_store, &scope, &candidate)
                    .await
                {
                    return Ok(HttpResponse::InternalServerError().json(json!({
                        "error": "task_note_memory_candidate_routing_failed",
                        "message": error.to_string(),
                        "candidate_id": candidate.id,
                    })));
                }
            }
            match api.learning_store.read_candidate(&scope, &candidate_id) {
                Ok(candidate) => Ok(HttpResponse::Created().json(json!({
                    "candidate": candidate,
                    "note": note,
                }))),
                Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
                    "error": "task_note_memory_candidate_read_failed",
                    "message": error.to_string(),
                }))),
            }
        },
        Err(error) => Ok(HttpResponse::Conflict().json(json!({
            "error": "task_note_memory_candidate_failed",
            "message": error.to_string(),
        }))),
    }
}

fn task_artifact_error_response(error: ArtifactV2Error) -> HttpResponse {
    match error {
        ArtifactV2Error::TaskNotFound(task_id) => HttpResponse::NotFound().json(json!({
            "error": "task_not_found",
            "task_id": task_id,
        })),
        ArtifactV2Error::InvalidRequest(message) => HttpResponse::BadRequest().json(json!({
            "error": "invalid_task_note_request",
            "message": message,
        })),
        other => HttpResponse::InternalServerError().json(json!({
            "error": "task_note_projection_failed",
            "message": other.to_string(),
        })),
    }
}

fn notes_io_error_response(error: std::io::Error) -> HttpResponse {
    let status = match error.kind() {
        std::io::ErrorKind::InvalidInput => actix_web::http::StatusCode::BAD_REQUEST,
        std::io::ErrorKind::PermissionDenied => actix_web::http::StatusCode::FORBIDDEN,
        std::io::ErrorKind::NotFound => actix_web::http::StatusCode::NOT_FOUND,
        std::io::ErrorKind::AlreadyExists => actix_web::http::StatusCode::CONFLICT,
        std::io::ErrorKind::WouldBlock
        | std::io::ErrorKind::TimedOut
        | std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected => actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
        _ => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpResponse::build(status).json(json!({
        "error": "notes_io_error",
        "message": error.to_string(),
    }))
}

fn normalize_open_root_provider(value: &str) -> Option<&'static str> {
    match value.trim() {
        "local" | "local_markdown" => Some("local_markdown"),
        "sb" | "silverbullet" | "silverbullet_space" => Some("silverbullet"),
        _ => None,
    }
}

pub fn configure_notes_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/notes")
            .route("/settings", web::get().to(get_notes_settings_handler))
            .route("/settings", web::put().to(put_notes_settings_handler))
            .route(
                "/providers/status",
                web::get().to(notes_provider_status_handler),
            )
            .route(
                "/providers/open-root",
                web::post().to(open_notes_provider_root_handler),
            )
            .route("", web::post().to(create_note_handler))
            .route(
                "/publish/task/{task_id}",
                web::post().to(publish_task_note_handler),
            )
            .route(
                "/publish/tasks/backfill",
                web::post().to(backfill_task_notes_handler),
            )
            .route("/published-tasks", web::get().to(list_task_notes_handler))
            .route(
                "/published-tasks/{task_id}",
                web::get().to(get_task_note_handler),
            )
            .route(
                "/published-tasks/{task_id}/promote-memory",
                web::post().to(promote_task_note_to_memory_handler),
            )
            .route("/audio", web::get().to(list_audio_notes_handler))
            .route("/audio", web::post().to(create_audio_note_handler))
            .route("/audio/{note_id}", web::get().to(get_audio_note_handler))
            .route(
                "/audio/{note_id}/recording",
                web::get().to(get_audio_note_recording_handler),
            )
            .route(
                "/audio/{note_id}",
                web::delete().to(delete_audio_note_handler),
            )
            .route("/append", web::post().to(append_note_handler))
            .route("/tree", web::get().to(list_note_tree_handler))
            .route("/tree", web::post().to(create_note_folder_handler))
            .route("/tree", web::delete().to(delete_note_folder_handler))
            .route("/file", web::get().to(read_note_file_handler))
            .route("/file", web::post().to(create_markdown_note_handler))
            .route("/file", web::put().to(save_note_markdown_handler))
            .route("/file", web::delete().to(delete_markdown_note_handler))
            .route("/backlinks", web::get().to(list_note_backlinks_handler))
            .route("/search", web::post().to(search_notes_handler))
            .route(
                "/capture-selection",
                web::post().to(capture_selection_handler),
            ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published_note() -> TaskNoteIndexEntry {
        TaskNoteIndexEntry {
            projection_id: "task:task-1".to_string(),
            schema: "magician.task-note.v1".to_string(),
            task_id: "task-1".to_string(),
            title: "Launch brief".to_string(),
            status: "completed".to_string(),
            agent_id: "researcher".to_string(),
            thread_id: "thread-1".to_string(),
            mode: TaskNotePublishMode::Standard,
            requested_provider: "silverbullet".to_string(),
            provider: "silverbullet".to_string(),
            used_fallback: false,
            fallback_reason: None,
            task_created_at: "2026-08-01T09:00:00Z".to_string(),
            task_due_date: Some("2026-08-03".to_string()),
            task_completed_at: Some("2026-08-01T10:00:00Z".to_string()),
            source_updated_at: "2026-08-01T10:00:00Z".to_string(),
            published_at: "2026-08-01T10:01:00Z".to_string(),
            date: "2026-08-01".to_string(),
            tags: vec!["magician/task".to_string(), "date/2026-08-01".to_string()],
            note_path: "Tasks/2026-08-01/task-1-launch-brief.md".to_string(),
            open_url: Some("https://notes.example.test/Tasks/2026-08-01/task-1".to_string()),
            assets: Vec::new(),
        }
    }

    #[test]
    fn memory_candidate_content_ignores_volatile_publication_metadata_and_is_bounded() {
        let first = "---\npublished_at: one\n---\n\n# Note\n\n> completed · source · published one\n\n## Final answer\n\nDurable fact";
        let second = "---\npublished_at: two\n---\n\n# Note\n\n> completed · source · published two\n\n## Final answer\n\nDurable fact";
        let first_value = bounded_note_memory_value(first, 10_000);
        let second_value = bounded_note_memory_value(second, 10_000);
        assert_eq!(first_value, second_value);
        assert!(!first_value.contains("published one"));
        assert_eq!(bounded_note_memory_value("abcdef", 3), "abc");
        assert_eq!(
            task_note_memory_candidate_id("task-1", "source-1", &first_value),
            task_note_memory_candidate_id("task-1", "source-1", &second_value)
        );
        assert_ne!(
            task_note_memory_candidate_id("task-1", "source-1", &first_value),
            task_note_memory_candidate_id("task-1", "source-1", "different")
        );
    }

    #[test]
    fn note_promotion_builds_an_explicit_review_gated_user_memory_upsert() {
        let request = build_task_note_memory_candidate_request(
            &published_note(),
            "task-1",
            "Durable launch fact".to_string(),
        );
        assert_eq!(request.candidate_type, LearningCandidateType::MemoryFact);
        assert_eq!(request.state, LearningCandidateState::Proposed);
        assert!(request.review_required);
        assert_eq!(request.proposed_target.as_deref(), Some("user.knowledge"));
        assert_eq!(request.promotion_target.as_deref(), Some("user.knowledge"));
        assert_eq!(request.proposed_change["memory"]["scope"], "user");
        assert_eq!(
            request.proposed_change["memory"]["target_tier"],
            "knowledge"
        );
        assert_eq!(request.proposed_change["memory"]["operation"], "upsert");
        assert_eq!(
            request.proposed_change["memory"]["explicit_user_request"],
            true
        );
        assert_eq!(request.evidence_refs[0].kind, "task_note");
    }

    #[tokio::test]
    async fn note_promotion_routes_to_review_without_writing_memory() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let store = LearningStore::new(workspace.clone());
        let bridge = LearningMemoryBridge::new(workspace);
        let scope = LearningScope::new("anonymous", "default");
        let note = published_note();
        let value = "Durable launch fact".to_string();
        let id = task_note_memory_candidate_id("task-1", &note.source_updated_at, &value);
        let candidate = store
            .ensure_candidate_with_id(
                scope.clone(),
                build_task_note_memory_candidate_request(&note, "task-1", value),
                &id,
            )
            .unwrap();

        let outcome = bridge
            .route_candidate(&store, &scope, &candidate)
            .await
            .unwrap();

        assert!(outcome.routed);
        assert!(!outcome.promoted);
        assert_eq!(outcome.target.as_deref(), Some("user.knowledge"));
        assert_eq!(
            store.read_candidate(&scope, &id).unwrap().state,
            LearningCandidateState::Triaged
        );
    }
}
