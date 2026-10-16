//! Authenticated admission and playback receipts for concurrent personal voice.
use super::*;
use magician::magician_v2::chat::voice_requests::{
    VoiceAdmission, VoiceMutation, VoicePlaybackEvent,
};

#[derive(Deserialize)]
pub struct ConcurrentVoiceBody {
    pub submission_id: String,
    #[serde(default)]
    pub context_session_id: Option<String>,
    #[serde(flatten)]
    pub message: SendMessageRequest,
}

#[derive(Deserialize)]
pub struct ConcurrentVoiceQuery {
    pub workspace: Option<String>,
}

pub(super) fn voice_scope(
    req: &HttpRequest,
    workspace: Option<String>,
) -> Result<(String, String), HttpResponse> {
    let Some(identity) = req
        .extensions()
        .get::<magician::magician_v2::cloudflare_access::VerifiedRequestIdentity>()
        .cloned()
    else {
        return Err(HttpResponse::Unauthorized().finish());
    };
    let workspace = resolve_required_workspace(req.headers(), workspace)?;
    if identity
        .workspace()
        .is_some_and(|expected| expected != workspace)
    {
        return Err(HttpResponse::Forbidden().finish());
    }
    Ok((identity.principal().to_string(), workspace))
}

pub(super) fn voice_error(error: anyhow::Error) -> HttpResponse {
    let message = error.to_string();
    let status = if message.contains("capacity") || message.contains("backlog_full") {
        actix_web::http::StatusCode::TOO_MANY_REQUESTS
    } else if message.contains("not_found") {
        actix_web::http::StatusCode::NOT_FOUND
    } else if message.contains("scope_mismatch") || message.contains("parent_mismatch") {
        actix_web::http::StatusCode::FORBIDDEN
    } else if message.starts_with("invalid_") {
        actix_web::http::StatusCode::BAD_REQUEST
    } else {
        actix_web::http::StatusCode::CONFLICT
    };
    HttpResponse::build(status).json(serde_json::json!({ "error": message }))
}

pub async fn submit_concurrent_voice_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ConcurrentVoiceQuery>,
    body: web::Json<ConcurrentVoiceBody>,
) -> HttpResponse {
    let (principal, workspace) = match voice_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let session_id = path.into_inner();
    if let Some(response) =
        ensure_session_in_scope(api.get_ref(), &session_id, &principal, &workspace).await
    {
        return response;
    }
    let body = body.into_inner();
    let text = match validate_send_message_request(&body.message) {
        Ok(Some(text)) => text.to_string(),
        Ok(None) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error":"voice_text_required"}))
        },
        Err(response) => return response,
    };
    if !body.message.attachment_ids.is_empty()
        || body.message.plan_task_id.is_some()
        || body.message.plan_question_id.is_some()
    {
        return HttpResponse::BadRequest().json(serde_json::json!({"error":"Use the addressed chat action for attachments or a pending plan answer."}));
    }
    if let Some(response) = validate_profile_override(api.get_ref(), &body.message) {
        return response;
    }
    let profile = match routed_profile_override(&body.message) {
        Ok(profile) => profile,
        Err(response) => return response,
    };
    let session = match api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(error) => return voice_error(error),
    };
    let credential = match app_owner_execution_credential_for_request(&req, &session) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let admission = VoiceAdmission {
        submission_id: body.submission_id,
        text,
        context_session_id: body.context_session_id,
        profile,
        mode: body.message.mode,
        coding_choice:
            magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(
                body.message.coding_choice,
                None,
            ),
        source_surface: "web".to_string(),
        presence_session_id: body.message.presence_session_id,
        executor_id: String::new(),
    };
    match api
        .chat_service
        .submit_concurrent_voice_request(&session_id, admission, credential)
        .await
    {
        Ok(receipt) => HttpResponse::Accepted().json(receipt),
        Err(error) => voice_error(error),
    }
}

pub async fn list_concurrent_voice_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ConcurrentVoiceQuery>,
) -> HttpResponse {
    let (principal, workspace) = match voice_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .chat_service
        .concurrent_voice_state(&principal, &workspace)
        .await
    {
        Ok(state) => HttpResponse::Ok().json(state),
        Err(error) => voice_error(error),
    }
}

pub async fn cancel_concurrent_voice_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ConcurrentVoiceQuery>,
) -> HttpResponse {
    let (principal, workspace) = match voice_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .chat_service
        .cancel_concurrent_voice_request(&principal, &workspace, &path.into_inner())
        .await
    {
        Ok(state) => HttpResponse::Ok().json(state),
        Err(error) => voice_error(error),
    }
}

/// The client can control presentation, never forge execution completion.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceDeliveryCommand {
    Acquire {
        device_id: String,
        interaction_id: String,
    },
    Release {
        device_id: String,
        interaction_id: String,
        epoch: u64,
    },
    Claim {
        request_id: String,
        device_id: String,
        interaction_id: String,
        epoch: u64,
        focus_epoch: u64,
        attempt_id: String,
        #[serde(default)]
        replay: bool,
    },
    Playback {
        request_id: String,
        device_id: String,
        interaction_id: String,
        epoch: u64,
        attempt_id: String,
        event: VoicePlaybackEvent,
    },
    Read {
        request_id: String,
    },
    Dismiss {
        request_id: String,
    },
}

impl From<VoiceDeliveryCommand> for VoiceMutation {
    fn from(command: VoiceDeliveryCommand) -> Self {
        match command {
            VoiceDeliveryCommand::Acquire {
                device_id,
                interaction_id,
            } => Self::AcquireOutput {
                device_id,
                interaction_id,
            },
            VoiceDeliveryCommand::Release {
                device_id,
                interaction_id,
                epoch,
            } => Self::ReleaseOutput {
                device_id,
                interaction_id,
                epoch,
            },
            VoiceDeliveryCommand::Claim {
                request_id,
                device_id,
                interaction_id,
                epoch,
                focus_epoch,
                attempt_id,
                replay,
            } => Self::Claim {
                request_id,
                device_id,
                interaction_id,
                epoch,
                focus_epoch,
                attempt_id,
                replay,
            },
            VoiceDeliveryCommand::Playback {
                request_id,
                device_id,
                interaction_id,
                epoch,
                attempt_id,
                event,
            } => Self::Playback {
                request_id,
                device_id,
                interaction_id,
                epoch,
                attempt_id,
                event,
            },
            VoiceDeliveryCommand::Read { request_id } => Self::Read { request_id },
            VoiceDeliveryCommand::Dismiss { request_id } => Self::Dismiss { request_id },
        }
    }
}

pub async fn concurrent_voice_delivery_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ConcurrentVoiceQuery>,
    body: web::Json<VoiceDeliveryCommand>,
) -> HttpResponse {
    let (principal, workspace) = match voice_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .chat_service
        .mutate_concurrent_voice_delivery(&principal, &workspace, body.into_inner().into())
        .await
    {
        Ok(state) => HttpResponse::Ok().json(state),
        Err(error) => voice_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_wire_cannot_mint_execution_status_or_internal_commands() {
        for body in [
            r#"{"action":"finish","request_id":"a","status":"completed"}"#,
            r#"{"action":"recover","executor_id":"forged"}"#,
            r#"{"action":"dismiss","request_id":"a","principal":"another"}"#,
        ] {
            assert!(serde_json::from_str::<VoiceDeliveryCommand>(body).is_err());
        }
    }
    #[test]
    fn unauthenticated_voice_admission_cannot_use_scope_headers_as_authority() {
        let req = actix_web::test::TestRequest::default()
            .insert_header(("x-magician-principal", "owner"))
            .to_http_request();
        assert!(voice_scope(&req, Some("default".into())).is_err());
    }
}

pub async fn concurrent_voice_result_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ConcurrentVoiceQuery>,
) -> HttpResponse {
    let (principal, workspace) = match voice_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .chat_service
        .concurrent_voice_result(&principal, &workspace, &path.into_inner())
        .await
    {
        Ok(result) => HttpResponse::Ok().json(result),
        Err(error) => voice_error(error),
    }
}
