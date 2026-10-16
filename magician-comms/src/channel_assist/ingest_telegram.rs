//! Telegram bot chat ingestor for the comms-assist data plane.
//!
//! The normal Telegram bot already persists inbound/outbound conversation turns
//! in the chat store. This adapter projects those chat-session text turns into
//! the same metadata-only rows used by Kapso, then re-fetches text from the
//! chat store at distillation time. Raw message text is never written to the
//! comms-assist DuckDB store.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use async_trait::async_trait;

use magician::magician_v2::chat::models::{
    ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSession,
};
use magician::magician_v2::chat::storage::{ChatStore, FileChatStore};

use super::assist::content::{prepare_for_distill, DistillContent};
use super::assist::distill::{ContentFetcher, DistillContext};
use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::registry::ChannelAccount;
use super::sensitivity::is_sensitive;
use super::types::{
    DistillState, MailMessageMeta, MailRecordOrigin, MailThreadRecord, MessageDirection,
    SyncWatermark, MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};

pub const TELEGRAM_PROVIDER: &str = "telegram";

const TELEGRAM_CHANNEL_TYPE: &str = "telegram";
const TELEGRAM_ENVOY_SOURCE_SURFACE: &str = "telegram-envoy-chat";
const MESSAGE_PAGE_SIZE: usize = 200;
const MAX_MESSAGE_PAGES: usize = 20;

pub struct TelegramChatIngestor;
pub struct TelegramChatContentFetcher;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn nonempty_trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn message_text(message: &ChatMessage) -> Option<&str> {
    match &message.content {
        ChatMessageContent::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
        _ => None,
    }
}

fn message_direction(message: &ChatMessage) -> Option<MessageDirection> {
    match &message.direction {
        ChatMessageDirection::User => Some(MessageDirection::Inbound),
        ChatMessageDirection::Assistant => Some(MessageDirection::Outbound),
        ChatMessageDirection::System => None,
    }
}

fn is_ordinary_telegram_envoy_session(session: &ChatSession) -> bool {
    session.origin_channel.channel_type == TELEGRAM_CHANNEL_TYPE
        && !session.ui_thread_id.trim().ends_with(":magic")
}

fn is_ordinary_telegram_envoy_message(message: &ChatMessage) -> bool {
    message
        .source_surface
        .as_deref()
        .map(str::trim)
        .is_some_and(|surface| surface == TELEGRAM_ENVOY_SOURCE_SURFACE)
}

fn telegram_session_subject(session: &ChatSession) -> String {
    nonempty_trimmed(session.title.as_deref())
        .or_else(|| nonempty_trimmed(session.origin_channel.address.as_deref()))
        .unwrap_or_else(|| session.ui_thread_id.clone())
}

fn telegram_from_name(session: &ChatSession, direction: MessageDirection) -> Option<String> {
    match direction {
        MessageDirection::Inbound => Some(telegram_session_subject(session)),
        MessageDirection::Outbound => {
            Some(magician::magician_v2::presentation_identity::ASSISTANT_FALLBACK_NAME.to_string())
        },
    }
}

fn telegram_from_address(session: &ChatSession, direction: MessageDirection) -> Option<String> {
    match direction {
        MessageDirection::Inbound => session.origin_channel.address.clone(),
        MessageDirection::Outbound => Some("telegram:magician".to_string()),
    }
}

async fn open_chat_store(ctx: &IngestContext) -> Result<FileChatStore> {
    FileChatStore::with_workspace_layout_index(ctx.workspace_layout.clone()).await
}

async fn open_distill_chat_store(ctx: &DistillContext) -> Result<FileChatStore> {
    FileChatStore::with_workspace_layout_index(ctx.workspace_layout.clone()).await
}

async fn recent_text_messages(
    store: &FileChatStore,
    session_id: &str,
    lower_bound_ms: i64,
) -> Result<Vec<ChatMessage>> {
    let mut before_id: Option<String> = None;
    let mut rows = Vec::new();
    for _ in 0..MAX_MESSAGE_PAGES {
        let (page, has_more) = store
            .get_messages_paginated(session_id, MESSAGE_PAGE_SIZE, before_id.as_deref())
            .await?;
        if page.is_empty() {
            break;
        }
        let next_before_id = page.first().map(|message| message.id.clone());
        let page_reached_floor = page
            .first()
            .map(|message| message.created_at < lower_bound_ms)
            .unwrap_or(false);
        for message in page {
            if message.created_at < lower_bound_ms {
                continue;
            }
            if is_ordinary_telegram_envoy_message(&message)
                && message_direction(&message).is_some()
                && message_text(&message).is_some()
            {
                rows.push(message);
            }
        }
        if !has_more || page_reached_floor {
            break;
        }
        before_id = next_before_id;
        if before_id.is_none() {
            break;
        }
    }
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

async fn find_text_message(
    store: &FileChatStore,
    session_id: &str,
    message_id: &str,
) -> Result<Option<String>> {
    let mut before_id: Option<String> = None;
    for _ in 0..MAX_MESSAGE_PAGES {
        let (page, has_more) = store
            .get_messages_paginated(session_id, MESSAGE_PAGE_SIZE, before_id.as_deref())
            .await?;
        if page.is_empty() {
            break;
        }
        if let Some(text) = page.iter().find_map(|message| {
            if message.id == message_id {
                message_text(message).map(str::to_string)
            } else {
                None
            }
        }) {
            return Ok(Some(text));
        }
        before_id = page.first().map(|message| message.id.clone());
        if !has_more || before_id.is_none() {
            break;
        }
    }
    Ok(None)
}

fn map_session_to_batch(
    session: &ChatSession,
    messages: Vec<ChatMessage>,
    account: &ChannelAccount,
    suppress: bool,
    observed_at: i64,
) -> IngestBatch {
    let subject = telegram_session_subject(session);
    let thread_sensitive =
        suppress && is_sensitive(Some(&subject), session.origin_channel.address.as_deref());
    let mut rows = Vec::with_capacity(messages.len());
    let mut latest_millis = 0;
    let mut latest_from_name = None;
    let mut latest_from_address = None;

    for message in messages {
        let Some(direction) = message_direction(&message) else {
            continue;
        };
        latest_millis = latest_millis.max(message.created_at);
        if message.created_at >= latest_millis {
            latest_from_name = telegram_from_name(session, direction);
            latest_from_address = telegram_from_address(session, direction);
        }
        rows.push(MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: TELEGRAM_PROVIDER.to_string(),
            account_alias: account.account_alias.clone(),
            account_email: None,
            thread_id: session.id.clone(),
            message_id: message.id,
            provider_cursor: Some(message.created_at.to_string()),
            label_ids: Vec::new(),
            subject: None,
            from_name: telegram_from_name(session, direction),
            from_address: telegram_from_address(session, direction),
            to_domains: Vec::new(),
            cc_domains: Vec::new(),
            internal_date: message.created_at,
            observed_at,
            direction: Some(direction),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: if thread_sensitive {
                DistillState::Suppressed
            } else {
                DistillState::Pending
            },
            distill_attempts: 0,
            sensitive_suppressed: thread_sensitive,
            origin: MailRecordOrigin::MetadataSync,
        });
    }

    let mut batch = IngestBatch::default();
    if rows.is_empty() {
        return batch;
    }

    batch.threads.push(MailThreadRecord {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: TELEGRAM_PROVIDER.to_string(),
        account_alias: account.account_alias.clone(),
        account_email: None,
        thread_id: session.id.clone(),
        lane: account.lane,
        subject: Some(if thread_sensitive {
            REDACTED_SUBJECT_PLACEHOLDER.to_string()
        } else {
            subject
        }),
        latest_summary: None,
        latest_from_name,
        latest_from_address,
        recipient_domains: Vec::new(),
        label_ids: Vec::new(),
        message_count: rows.len() as i64,
        last_message_at: Some(latest_millis),
        provider_cursor: Some(latest_millis.to_string()),
        sensitive_suppressed: thread_sensitive,
        origin: MailRecordOrigin::MetadataSync,
        first_observed_at: observed_at,
        last_observed_at: observed_at,
    });
    batch.max_internal_date = Some(latest_millis);
    batch.next_provider_cursor = Some(latest_millis.to_string());
    batch.messages = rows;
    batch
}

#[async_trait]
impl ChannelIngestor for TelegramChatIngestor {
    fn provider(&self) -> &'static str {
        TELEGRAM_PROVIDER
    }

    async fn sync_account(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        watermark: Option<&SyncWatermark>,
    ) -> Result<IngestBatch> {
        let lower_bound_ms = watermark
            .and_then(|watermark| watermark.last_internal_date)
            .unwrap_or(ctx.min_internal_date)
            .max(ctx.min_internal_date);
        let observed_at = now_millis();
        let store = open_chat_store(ctx).await?;
        let sessions = store.list_sessions(&ctx.principal, &ctx.workspace).await?;
        let mut batch = IngestBatch {
            mode: if watermark.is_some() {
                "incremental"
            } else {
                "backfill"
            },
            ..IngestBatch::default()
        };
        let mut threads_seen = 0usize;
        let mut capped = false;

        for session in sessions {
            if session.updated_at < lower_bound_ms {
                break;
            }
            if !is_ordinary_telegram_envoy_session(&session) {
                continue;
            }
            if threads_seen >= ctx.max_threads {
                capped = true;
                break;
            }
            let messages = recent_text_messages(&store, &session.id, lower_bound_ms).await?;
            if messages.is_empty() {
                continue;
            }
            let session_batch = map_session_to_batch(
                &session,
                messages,
                account,
                ctx.suppress_sensitive,
                observed_at,
            );
            if session_batch.messages.is_empty() {
                continue;
            }
            threads_seen += 1;
            batch.threads.extend(session_batch.threads);
            batch.messages.extend(session_batch.messages);
            if let Some(max_internal_date) = session_batch.max_internal_date {
                batch.max_internal_date =
                    Some(batch.max_internal_date.unwrap_or(0).max(max_internal_date));
            }
            if let Some(cursor) = session_batch
                .next_provider_cursor
                .as_deref()
                .and_then(|cursor| cursor.parse::<i64>().ok())
            {
                let next = batch
                    .next_provider_cursor
                    .as_deref()
                    .and_then(|cursor| cursor.parse::<i64>().ok())
                    .unwrap_or(0)
                    .max(cursor);
                batch.next_provider_cursor = Some(next.to_string());
            }
        }

        if capped {
            batch.max_internal_date = None;
            batch.next_provider_cursor = None;
        }
        Ok(batch)
    }
}

#[async_trait]
impl ContentFetcher for TelegramChatContentFetcher {
    fn provider(&self) -> &'static str {
        TELEGRAM_PROVIDER
    }

    async fn fetch(
        &self,
        ctx: &DistillContext,
        message: &MailMessageMeta,
    ) -> Result<DistillContent> {
        let store = open_distill_chat_store(ctx).await?;
        let Some(text) = find_text_message(&store, &message.thread_id, &message.message_id).await?
        else {
            return Ok(DistillContent::default());
        };
        let prepared = prepare_for_distill(&text, ctx.chunk_chars, ctx.max_chunks);
        Ok(DistillContent {
            chunks: prepared.chunks,
            truncated: prepared.truncated,
            had_html: false,
            attachment_count: 0,
        })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::channel_assist::types::ChannelLane;
    use magician::magician_v2::chat::models::ChatChannel;

    fn session() -> ChatSession {
        ChatSession {
            internal_voice: None,
            id: "session-1".to_string(),
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            agent_id: "envoy".to_string(),
            ui_thread_id: "ext:telegram:123".to_string(),
            title: Some("Alice".to_string()),
            origin_channel: ChatChannel::new("telegram", "123"),
            status: magician::magician_v2::chat::models::ChatSessionStatus::Active,
            history_lane: magician::magician_v2::history::HistoryLane::Automated,
            is_default_session: false,
            created_at: 1,
            updated_at: 2,
        }
    }

    fn text_message(id: &str, direction: ChatMessageDirection, created_at: i64) -> ChatMessage {
        ChatMessage {
            id: id.to_string(),
            session_id: "session-1".to_string(),
            direction,
            content: ChatMessageContent::Text {
                text: "hello".to_string(),
                plan_reply: None,
            },
            created_at,
            chat_turn_id: None,
            source_surface: Some(TELEGRAM_ENVOY_SOURCE_SURFACE.to_string()),
            presence_session_id: None,
            voice_origin: None,
            context_origin: None,
            speech_segments: None,
            presentation: None,
        }
    }

    #[test]
    fn maps_telegram_session_messages_to_envoy_rows() {
        let account = ChannelAccount {
            provider: TELEGRAM_PROVIDER.to_string(),
            account_alias: "presto".to_string(),
            lane: ChannelLane::Envoy,
            enabled: true,
        };
        let batch = map_session_to_batch(
            &session(),
            vec![
                text_message("u1", ChatMessageDirection::User, 1000),
                text_message("a1", ChatMessageDirection::Assistant, 2000),
            ],
            &account,
            true,
            3000,
        );

        assert_eq!(batch.threads.len(), 1);
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.threads[0].provider, TELEGRAM_PROVIDER);
        assert_eq!(batch.threads[0].account_alias, "presto");
        assert_eq!(batch.threads[0].lane, ChannelLane::Envoy);
        assert_eq!(batch.threads[0].subject.as_deref(), Some("Alice"));
        assert_eq!(batch.messages[0].direction, Some(MessageDirection::Inbound));
        assert_eq!(
            batch.messages[1].direction,
            Some(MessageDirection::Outbound)
        );
    }

    #[test]
    fn ordinary_telegram_envoy_session_filter_excludes_magic_threads() {
        let ordinary = session();
        assert!(is_ordinary_telegram_envoy_session(&ordinary));

        let mut magic = session();
        magic.ui_thread_id = "ext:telegram:123:magic".to_string();
        assert!(!is_ordinary_telegram_envoy_session(&magic));

        let mut kapso = session();
        kapso.origin_channel = ChatChannel::new("kapso", "123");
        assert!(!is_ordinary_telegram_envoy_session(&kapso));
    }

    #[test]
    fn ordinary_telegram_message_filter_requires_envoy_source_surface() {
        let ordinary = text_message("u1", ChatMessageDirection::User, 1000);
        assert!(is_ordinary_telegram_envoy_message(&ordinary));

        let mut missing_surface = text_message("u2", ChatMessageDirection::User, 2000);
        missing_surface.source_surface = None;
        assert!(!is_ordinary_telegram_envoy_message(&missing_surface));

        let mut control_surface = text_message("u3", ChatMessageDirection::User, 3000);
        control_surface.source_surface = Some("telegram-control".to_string());
        assert!(!is_ordinary_telegram_envoy_message(&control_surface));
    }
}
