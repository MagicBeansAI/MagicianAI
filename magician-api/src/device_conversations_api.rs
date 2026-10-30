//! Conversations for a paired voice terminal (the ESP32), sized for a device
//! with 512 KB of RAM.
//!
//! - `GET  /api/magician/v2/devices/me/conversations?thread=<id>` — the
//!   owner's recent threads (at most [`MAX_THREADS`]) and the chosen thread's
//!   recent sessions (at most [`MAX_SESSIONS`]), so the terminal's screen can
//!   offer them.
//! - `POST /api/magician/v2/devices/me/threads` — start a thread from the
//!   terminal. It is named after the device and the time
//!   ("ESP32-C6 voice terminal · 25 Oct 22:10") and filed under Automated.
//!
//! Both answer only a request authenticated as a paired device; scope comes
//! from that device's pairing, never from the request.
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use chrono::Local;
use serde::Deserialize;
use serde_json::json;

use crate::{chat_api::ChatApi, scope::resolve_required_scope};
use magician::magician_v2::history::HistoryLane;
use magician::magician_v2::{
    archived_filter::ArchivedFilter, chat::storage::ChatSessionPageQuery,
    cloudflare_access::MOBILE_DEVICE_ID_HEADER, device_pairing::DevicePairingStore,
};

/// Threads offered on the terminal's screen.
pub const MAX_THREADS: usize = 8;
/// Sessions offered for the chosen thread.
pub const MAX_SESSIONS: usize = 6;
/// Names are cut to what the terminal's list rows can show.
const MAX_NAME_CHARS: usize = 40;

/// The paired device a request comes from, and its own default thread.
pub(crate) struct PairedTerminal {
    pub thread_id: String,
    pub label: String,
    pub slug: String,
}

pub(crate) async fn paired_terminal(
    req: &HttpRequest,
    principal: &str,
    workspace: &str,
) -> Option<PairedTerminal> {
    let device_id = req
        .headers()
        .get(MOBILE_DEVICE_ID_HEADER)?
        .to_str()
        .ok()?
        .trim();
    if device_id.is_empty() {
        return None;
    }
    let pairing = req.app_data::<web::Data<Arc<DevicePairingStore>>>()?;
    let device = pairing
        .list_mobile(principal, workspace)
        .await
        .into_iter()
        .find(|device| device.device_id == device_id)?;
    let slug: String = device
        .device_id
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = device.label.trim();
    Some(PairedTerminal {
        thread_id: format!("device-{slug}"),
        label: if label.is_empty() {
            "Voice terminal".to_string()
        } else {
            label.to_string()
        },
        slug,
    })
}

/// The longest id the ESP32 terminal stores whole (`CONV_ID_LEN` 72 in
/// `magesp/main/conv.h`, NUL included).
const DEVICE_MAX_ID_LEN: usize = 71;

fn short(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_NAME_CHARS {
        return trimmed.to_string();
    }
    let mut cut: String = trimmed.chars().take(MAX_NAME_CHARS - 1).collect();
    cut.push('…');
    cut
}

fn device_required() -> HttpResponse {
    HttpResponse::Unauthorized().json(json!({ "error": "paired_device_required" }))
}

#[derive(Debug, Deserialize)]
pub struct ConversationsQuery {
    #[serde(default)]
    pub thread: Option<String>,
}

/// `GET /api/magician/v2/devices/me/conversations`
pub async fn device_conversations_handler(
    req: HttpRequest,
    chat_api: web::Data<ChatApi>,
    query: web::Query<ConversationsQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(terminal) = paired_terminal(&req, &principal, &workspace).await else {
        return Ok(device_required());
    };
    let service = &chat_api.chat_service;
    let Some(threads) = service.ui_threads() else {
        return Ok(
            HttpResponse::ServiceUnavailable().json(json!({ "error": "threads_unavailable" }))
        );
    };
    let page = match threads
        .list_threads_page(
            &principal,
            &workspace,
            None,
            Some(false),
            "",
            MAX_THREADS,
            0,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => {
            return Ok(HttpResponse::InternalServerError()
                .json(json!({ "error": "threads_failed", "details": error.to_string() })))
        },
    };
    let thread = query
        .thread
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(&terminal.thread_id)
        .to_string();
    let sessions = service
        .list_sessions_page(
            &principal,
            &workspace,
            ChatSessionPageQuery {
                ui_thread_id: Some(thread.clone()),
                history_lane: None,
                archived: ArchivedFilter::Exclude,
                search: String::new(),
                limit: MAX_SESSIONS,
                offset: 0,
            },
        )
        .await
        .map(|page| page.sessions)
        .unwrap_or_default();
    // The terminal's own thread is offered even before it exists. Listing
    // never creates it: a read that wrote would bring back a thread the owner
    // deleted on every poll. The terminal's next voice note creates it.
    // A thread id the terminal cannot hold whole would name another thread.
    let mut threads_json: Vec<serde_json::Value> = page
        .threads
        .iter()
        .filter(|record| record.id.len() <= DEVICE_MAX_ID_LEN)
        .map(|record| {
            json!({
                "id": record.id,
                "name": short(&record.name),
                "lane": record.history_lane.as_str(),
            })
        })
        .collect();
    if !page
        .threads
        .iter()
        .any(|record| record.id == terminal.thread_id)
    {
        threads_json.insert(
            0,
            json!({
                "id": terminal.thread_id,
                "name": short(&terminal.label),
                "lane": HistoryLane::Personal.as_str(),
            }),
        );
    }
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(json!({
            "device_thread": terminal.thread_id,
            "thread": thread,
            "threads": threads_json,
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "title": short(session.title.as_deref().unwrap_or("Untitled chat")),
            })).collect::<Vec<_>>(),
        })))
}

/// `POST /api/magician/v2/devices/me/threads`
pub async fn create_device_thread_handler(
    req: HttpRequest,
    chat_api: web::Data<ChatApi>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(terminal) = paired_terminal(&req, &principal, &workspace).await else {
        return Ok(device_required());
    };
    let Some(threads) = chat_api.chat_service.ui_threads() else {
        return Ok(
            HttpResponse::ServiceUnavailable().json(json!({ "error": "threads_unavailable" }))
        );
    };
    let now = Local::now();
    let id = format!("device-{}-{}", terminal.slug, now.format("%Y%m%d%H%M%S"));
    let name = format!("{} · {}", terminal.label, now.format("%-d %b %H:%M"));
    match threads
        .create_thread_in_lane(&principal, &workspace, &id, &name, HistoryLane::Automated)
        .await
    {
        Ok(record) => Ok(HttpResponse::Created().json(json!({
            "id": record.id,
            "name": short(&record.name),
            "lane": record.history_lane.as_str(),
        }))),
        Err(error) => Ok(HttpResponse::BadRequest()
            .json(json!({ "error": "thread_create_failed", "details": error.to_string() }))),
    }
}

#[cfg(test)]
mod tests {
    use super::short;

    #[test]
    fn names_fit_the_terminal_rows() {
        assert_eq!(short("  General  "), "General");
        let long = "x".repeat(60);
        let cut = short(&long);
        assert_eq!(cut.chars().count(), 40);
        assert!(cut.ends_with('…'));
    }
}
