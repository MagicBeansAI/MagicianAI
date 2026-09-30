//! Authenticated composer controls. Queue mutations preserve the running stream.
use super::*;
use super::concurrent_voice::{voice_error, voice_scope, ConcurrentVoiceQuery};
use magician::magician_v2::chat::models::{ChatSessionStatus, QueuedMessage};

async fn queue_session(api: &ChatApi, req: &HttpRequest, id: &str, workspace: Option<String>) -> Result<ChatSession, HttpResponse> {
    let (principal, workspace) = voice_scope(req, workspace)?;
    if let Some(response) = ensure_session_in_scope(api, id, &principal, &workspace).await { return Err(response); }
    let session = api.chat_service.get_session(id).await.map_err(voice_error)?
        .ok_or_else(|| HttpResponse::NotFound().finish())?;
    if session.status != ChatSessionStatus::Active || matches!(session.internal_voice,
        Some(magician::magician_v2::chat::voice_requests::InternalVoiceSession::Coordinator { .. })) {
        return Err(HttpResponse::Conflict().json(serde_json::json!({"error":"This session cannot accept messages."})));
    }
    Ok(session)
}

pub async fn enqueue_composer_message_handler(
    api: web::Data<ChatApi>, req: HttpRequest, path: web::Path<String>,
    query: web::Query<ConcurrentVoiceQuery>, body: web::Json<SendMessageRequest>,
) -> HttpResponse {
    let id = path.into_inner();
    let session = match queue_session(&api, &req, &id, query.workspace.clone()).await { Ok(session) => session, Err(response) => return response };
    let text = match validate_send_message_request(&body) { Ok(text) => text.map(str::to_owned), Err(response) => return response };
    if body.mode == ChatMessageMode::Plan || body.plan_task_id.is_some() || body.plan_question_id.is_some() {
        return HttpResponse::BadRequest().json(serde_json::json!({"error":"Use the addressed planning action for plan messages."}));
    }
    if rejects_client_selected_protected_surface(body.source_surface.as_deref()) { return HttpResponse::Forbidden().finish(); }
    if let Some(response) = validate_profile_override(&api, &body) { return response; }
    let profile = match routed_profile_override(&body) { Ok(profile) => profile, Err(response) => return response };
    let credential = match app_owner_execution_credential_for_request(&req, &session) { Ok(value) => value, Err(response) => return response };
    let body = body.into_inner();
    let message = QueuedMessage {
        id: uuid::Uuid::new_v4().to_string(), session_id: id.clone(), text,
        attachment_ids: body.attachment_ids, profile_override: profile,
        mode: body.mode, chat_turn_id: body.chat_turn_id, voice_origin: body.voice_origin,
        source_surface: body.source_surface, presence_session_id: body.presence_session_id,
        sender_display_name: None, channel: None, channel_address: None,
        coding_choice: magician::magician_v2::vibedev::run_service::queued_coding_choice(
            magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(body.coding_choice, None).as_ref()),
        queued_at: Utc::now().timestamp_millis(),
    };
    match api.chat_service.enqueue_composer_message(message, credential).await {
        Ok(receipt) => HttpResponse::Accepted().json(serde_json::json!({"queued":receipt,"session_id":id})),
        Err(error) => voice_error(error),
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueAction { Parallel, StopAndSend }

pub async fn queued_message_action_handler(
    api: web::Data<ChatApi>, req: HttpRequest, path: web::Path<(String, String)>,
    query: web::Query<ConcurrentVoiceQuery>, body: web::Json<QueueAction>,
) -> HttpResponse {
    let (id, message_id) = path.into_inner();
    let session = match queue_session(&api, &req, &id, query.workspace.clone()).await { Ok(session) => session, Err(response) => return response };
    let credential = match app_owner_execution_credential_for_request(&req, &session) { Ok(value) => value, Err(response) => return response };
    match body.into_inner() {
        QueueAction::Parallel => match api.chat_service.run_queued_message_in_parallel(&id, &message_id, credential).await {
            Ok(request) => HttpResponse::Ok().json(serde_json::json!({"request":request})),
            Err(error) => voice_error(error),
        },
        QueueAction::StopAndSend => match api.chat_service.stop_and_send_queued_message(&id, &message_id).await {
            Ok(()) => HttpResponse::Ok().json(serde_json::json!({"ok":true})),
            Err(error) => voice_error(error),
        },
    }
}
