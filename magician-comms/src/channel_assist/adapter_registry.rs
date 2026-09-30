//! Provider adapter registry for the comms-assist data plane.
//!
//! Phase 1 keeps the existing `ChannelIngestor` and `ContentFetcher` traits
//! intact and introduces one place that knows which shipped providers expose
//! those capabilities. Phase 2 adds deep-link and connection-status adapters.
//! Phase 3 moves provider adapter ownership under `channel_assist::adapters`, so
//! sync/distill behavior stays identical while new providers stop needing edits
//! in generic worker loops. Phase 5 adds dormant realtime/outbound contracts
//! for chat-native providers; no shipped adapter exposes those capabilities yet.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::adapters::agentmail::AgentMailAdapter;
pub use super::adapters::gmail::GMAIL_PROVIDER;

use super::adapters::gmail::GmailAdapter;
use super::adapters::imessage::ImessageAdapter;
use super::adapters::telegram::TelegramAdapter;
use super::adapters::whatsapp_kapso::WhatsappKapsoAdapter;
use super::adapters::whatsapp_wu::WhatsappWuAdapter;
use super::assist::distill::ContentFetcher;
use super::ingest::{ChannelIngestor, IngestBatch, IngestContext};
use super::ingest_agentmail::AGENTMAIL_PROVIDER;
use super::ingest_imessage::IMESSAGE_PROVIDER;
use super::ingest_kapso::KAPSO_PROVIDER;
use super::ingest_telegram::TELEGRAM_PROVIDER;
use super::ingest_whatsapp::WHATSAPP_PROVIDER;
use super::registry::ChannelAccount;
use super::types::{ChannelLane, MessageDirection};

/// Registry descriptor for one provider. Classification (`email` vs `chat`)
/// and user-facing channel names are still sourced from `channel_providers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelProviderDescriptor {
    pub provider: &'static str,
    pub display_label: String,
    pub channel: String,
    pub channel_label: &'static str,
}

impl ChannelProviderDescriptor {
    fn for_provider(provider: &'static str) -> Self {
        Self {
            provider,
            display_label: default_provider_display_label(provider),
            channel: super::channel_providers::channel_for_provider(provider).to_string(),
            channel_label: super::channel_providers::channel_label(provider),
        }
    }
}

fn default_provider_display_label(provider: &str) -> String {
    match provider {
        GMAIL_PROVIDER => "Gmail".to_string(),
        AGENTMAIL_PROVIDER => "AgentMail (Presto)".to_string(),
        WHATSAPP_PROVIDER => "WhatsApp (yours)".to_string(),
        KAPSO_PROVIDER => "WhatsApp (Presto)".to_string(),
        TELEGRAM_PROVIDER => "Telegram (Presto)".to_string(),
        IMESSAGE_PROVIDER => "iMessage".to_string(),
        _ => provider.to_string(),
    }
}

/// Optional capabilities an adapter may expose. Pull ingest, content fetch,
/// deep links, and connection status are wired through current workers/APIs;
/// later phases can fill the remaining flags without changing the
/// worker-facing registry shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ChannelCapabilities {
    pub pull_ingest: bool,
    pub content_fetch: bool,
    pub deep_link: bool,
    pub connection_status: bool,
    pub realtime_events: bool,
    pub outbound_send: bool,
    pub draft_create: bool,
    pub attachments: bool,
    pub reactions: bool,
    pub edits_and_deletes: bool,
}

impl ChannelCapabilities {
    pub const fn pull_with_content() -> Self {
        Self {
            pull_ingest: true,
            content_fetch: true,
            deep_link: false,
            connection_status: false,
            realtime_events: false,
            outbound_send: false,
            draft_create: false,
            attachments: false,
            reactions: false,
            edits_and_deletes: false,
        }
    }

    pub const fn with_deep_link(mut self) -> Self {
        self.deep_link = true;
        self
    }

    pub const fn with_connection_status(mut self) -> Self {
        self.connection_status = true;
        self
    }

    /// Enable outbound send + draft creation (a channel that ships a
    /// [`ChannelActionAdapter`] whose reply action can both compose a draft and
    /// send it).
    pub const fn with_outbound_send_and_draft(mut self) -> Self {
        self.outbound_send = true;
        self.draft_create = true;
        self
    }
}

/// Minimal provider-neutral reference for building provider-native thread
/// URLs. This mirrors the fields already projected into Follow-up cards and
/// avoids forcing API code to synthesize a full `MailThreadRecord`.
#[derive(Debug, Clone, Copy)]
pub struct ChannelThreadRef<'a> {
    pub provider: &'a str,
    pub account_alias: &'a str,
    pub account_email: Option<&'a str>,
    pub thread_id: &'a str,
}

/// Provider-neutral identity envelope for chat-native channels. These fields
/// let future realtime adapters map Slack/Telegram-style participants and
/// messages into the existing normalized thread/message rows without abusing
/// email-only fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelIdentity {
    /// Stable provider conversation id before Magician maps it to `thread_id`.
    pub external_conversation_id: String,
    /// Stable provider message id when the event refers to a concrete message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_message_id: Option<String>,
    /// Stable provider sender/actor id when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_external_id: Option<String>,
    /// Human display name for UI/debugging only; not treated as identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_display_name: Option<String>,
    /// Direction relative to the account owner, when the adapter can derive it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<MessageDirection>,
}

/// Normalized realtime event kinds. These are intentionally small until a
/// concrete provider proves a richer event model is required.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRealtimeEventKind {
    MessageAdded,
    MessageEdited,
    MessageDeleted,
    ReactionAdded,
    ReactionRemoved,
}

/// Compact reaction metadata. Raw message bodies and attachment payloads do not
/// belong in this envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelReaction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_display_name: Option<String>,
}

/// Provider-normalized realtime event. Future event-driven adapters can turn
/// this into an [`IngestBatch`] while the store remains row-oriented.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelRealtimeEvent {
    pub provider: String,
    pub account_alias: String,
    #[serde(default)]
    pub lane: ChannelLane,
    pub kind: ChannelRealtimeEventKind,
    pub identity: ChannelIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at_ms: Option<i64>,
    pub observed_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<ChannelReaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

/// A channel action a provider adapter can offer (reply, react, forward, …).
/// This supersedes the dormant `ChannelOutboundMode` Draft/Send enum: the old
/// Draft mode maps to [`ChannelActionAdapter::compose`] (produce an editable
/// draft) and Send maps to [`ChannelActionAdapter::commit`] (execute).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelActionDescriptor {
    /// Stable action id the API/UI uses to invoke `compose`/`commit`.
    pub id: String,
    /// Human-facing label for the action button.
    pub label: String,
    /// Whether the action first produces an editable draft (`compose`) before
    /// `commit`. `false` ⇒ the action commits directly (no LLM draft step).
    pub needs_compose: bool,
    /// Whether the UI should require explicit confirmation before `commit`.
    pub confirm: bool,
    /// Optional UI icon hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// An editable draft produced by [`ChannelActionAdapter::compose`]. The owner
/// reviews/edits `text`; the (possibly edited) text comes back as
/// [`ChannelActionRequest::body`] on the subsequent `commit`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelActionDraft {
    pub text: String,
}

/// Context for performing a channel action. `compose` needs the LLM router (it
/// dispatches the local reply-draft op the same way the classify/distill
/// workers do); `commit` reads its own host-gateway url from env. Both carry
/// the resolved scope for telemetry. Mirrors how `RouterReplyDraftLlm` is
/// constructed (router + broadcaster + principal/workspace).
pub struct ChannelActionContext {
    pub principal: String,
    pub workspace: String,
    pub router: Arc<OperationLlmRouter>,
    pub broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

/// Request contract for a channel action. Reuses the outbound-request shape
/// (`provider`, `account_alias`, `lane`, `identity`, `reply_to_message_id`,
/// `body`) and adds the compose draft-inputs the local reply-draft op needs.
/// The API handler (Task 5) fills the compose inputs from the store + content
/// fetcher so the adapter stays thin; `commit` only reads `body`/`identity`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelActionRequest {
    pub provider: String,
    pub account_alias: String,
    #[serde(default)]
    pub lane: ChannelLane,
    pub identity: ChannelIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<String>,
    /// The action body. On `commit` this is the (possibly edited) draft text.
    #[serde(default)]
    pub body: String,
    // ---- compose draft-inputs (populated by the API handler for `compose`) ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

/// Result of committing a channel action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelActionResult {
    pub provider: String,
    pub account_alias: String,
    /// The action id that was committed.
    pub action_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<serde_json::Value>,
}

pub trait ChannelDeepLinker: Send + Sync {
    fn thread_url(&self, thread: ChannelThreadRef<'_>) -> Option<String>;
}

pub trait ChannelConnectionStatus: Send + Sync {
    fn account_connected(&self, ctx: &IngestContext, account: &ChannelAccount) -> bool;
}

#[async_trait]
pub trait RealtimeChannelAdapter: Send + Sync {
    async fn handle_realtime_event(
        &self,
        ctx: &IngestContext,
        account: &ChannelAccount,
        event: ChannelRealtimeEvent,
    ) -> anyhow::Result<IngestBatch>;
}

/// Provider action boundary (supersedes the dormant `ChannelOutboundAdapter`).
/// An adapter advertises the actions it supports and performs them in two
/// stages: `compose` (produce an editable draft via the local LLM) and `commit`
/// (execute — e.g. send). Actions are keyed by `action_id`; an unknown id must
/// error.
#[async_trait]
pub trait ChannelActionAdapter: Send + Sync {
    /// The actions this adapter supports.
    fn available_actions(&self) -> Vec<ChannelActionDescriptor>;

    /// Produce an editable draft for an action that `needs_compose`.
    async fn compose(
        &self,
        action_id: &str,
        ctx: &ChannelActionContext,
        req: &ChannelActionRequest,
    ) -> anyhow::Result<ChannelActionDraft>;

    /// Execute the action. `req.body` is the (possibly edited) text.
    async fn commit(
        &self,
        action_id: &str,
        ctx: &ChannelActionContext,
        req: &ChannelActionRequest,
    ) -> anyhow::Result<ChannelActionResult>;
}

/// One provider adapter entry. Methods build fresh trait objects so generic
/// workers can keep owning their per-pass vectors just as they did before this
/// registry was introduced.
pub trait ChannelAdapter: Send + Sync {
    fn provider(&self) -> &'static str;

    fn descriptor(&self) -> ChannelProviderDescriptor {
        ChannelProviderDescriptor::for_provider(self.provider())
    }

    fn capabilities(&self) -> ChannelCapabilities;

    fn build_ingestor(&self) -> Option<Box<dyn ChannelIngestor>> {
        None
    }

    fn build_content_fetcher(&self) -> Option<Box<dyn ContentFetcher>> {
        None
    }

    fn deep_linker(&self) -> Option<&dyn ChannelDeepLinker> {
        None
    }

    fn connection_status(&self) -> Option<&dyn ChannelConnectionStatus> {
        None
    }

    fn realtime_adapter(&self) -> Option<&dyn RealtimeChannelAdapter> {
        None
    }

    fn action_adapter(&self) -> Option<&dyn ChannelActionAdapter> {
        None
    }
}

/// Shipped provider adapters. Adding a provider should add one adapter entry
/// here; generic workers consume the helpers below.
pub fn default_channel_adapters() -> Vec<Box<dyn ChannelAdapter>> {
    vec![
        Box::new(GmailAdapter),
        Box::new(WhatsappWuAdapter),
        Box::new(WhatsappKapsoAdapter),
        Box::new(TelegramAdapter),
        Box::new(AgentMailAdapter),
        Box::new(ImessageAdapter),
    ]
}

pub fn default_channel_ingestors() -> Vec<Box<dyn ChannelIngestor>> {
    default_channel_adapters()
        .into_iter()
        .filter_map(|adapter| adapter.build_ingestor())
        .collect()
}

pub fn default_channel_content_fetchers() -> Vec<Box<dyn ContentFetcher>> {
    default_channel_adapters()
        .into_iter()
        .filter_map(|adapter| adapter.build_content_fetcher())
        .collect()
}

pub fn descriptor_for(provider: &str) -> Option<ChannelProviderDescriptor> {
    default_channel_adapters()
        .iter()
        .find(|adapter| adapter.provider() == provider)
        .map(|adapter| adapter.descriptor())
}

pub fn capabilities_for(provider: &str) -> ChannelCapabilities {
    default_channel_adapters()
        .iter()
        .find(|adapter| adapter.provider() == provider)
        .map(|adapter| adapter.capabilities())
        .unwrap_or_default()
}

pub fn connection_status_for(
    ctx: &IngestContext,
    provider: &str,
    account_alias: &str,
    lane: ChannelLane,
) -> Option<bool> {
    let account = ChannelAccount {
        provider: provider.to_string(),
        account_alias: account_alias.to_string(),
        lane,
        enabled: true,
    };
    default_channel_adapters()
        .iter()
        .find(|adapter| adapter.provider() == provider)
        .and_then(|adapter| adapter.connection_status())
        .map(|status| status.account_connected(ctx, &account))
}

/// Status helper for `/channel-assist/sync/status`: Gmail reports local profile
/// readiness while non-Gmail channels
/// report whether the account is enabled in the resolved channel config.
pub fn sync_status_connected_for(
    ctx: &IngestContext,
    provider: &str,
    account_alias: &str,
    lane: ChannelLane,
    enabled_in_resolved_config: bool,
) -> bool {
    if provider == GMAIL_PROVIDER {
        connection_status_for(ctx, provider, account_alias, lane).unwrap_or(false)
    } else {
        enabled_in_resolved_config
    }
}

pub fn thread_url_for(thread: ChannelThreadRef<'_>) -> Option<String> {
    default_channel_adapters()
        .iter()
        .find(|adapter| adapter.provider() == thread.provider)
        .and_then(|adapter| adapter.deep_linker())
        .and_then(|linker| linker.thread_url(thread))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn sorted_providers(mut providers: Vec<&'static str>) -> Vec<&'static str> {
        providers.sort_unstable();
        providers
    }

    #[test]
    fn shipped_adapters_are_registered_once() {
        let adapters = default_channel_adapters();
        let providers: Vec<&'static str> = adapters.iter().map(|a| a.provider()).collect();
        let unique: HashSet<&'static str> = providers.iter().copied().collect();
        assert_eq!(unique.len(), providers.len());
        assert_eq!(
            sorted_providers(providers),
            sorted_providers(vec![
                AGENTMAIL_PROVIDER,
                GMAIL_PROVIDER,
                IMESSAGE_PROVIDER,
                KAPSO_PROVIDER,
                TELEGRAM_PROVIDER,
                WHATSAPP_PROVIDER,
            ])
        );
    }

    #[test]
    fn pull_ingestors_come_from_the_adapter_registry() {
        let providers = default_channel_ingestors()
            .iter()
            .map(|ingestor| ingestor.provider())
            .collect();
        assert_eq!(
            sorted_providers(providers),
            sorted_providers(vec![
                AGENTMAIL_PROVIDER,
                GMAIL_PROVIDER,
                IMESSAGE_PROVIDER,
                KAPSO_PROVIDER,
                TELEGRAM_PROVIDER,
                WHATSAPP_PROVIDER,
            ])
        );
    }

    #[test]
    fn content_fetchers_come_from_the_adapter_registry() {
        let providers = default_channel_content_fetchers()
            .iter()
            .map(|fetcher| fetcher.provider())
            .collect();
        assert_eq!(
            sorted_providers(providers),
            sorted_providers(vec![
                AGENTMAIL_PROVIDER,
                GMAIL_PROVIDER,
                IMESSAGE_PROVIDER,
                KAPSO_PROVIDER,
                TELEGRAM_PROVIDER,
                WHATSAPP_PROVIDER,
            ])
        );
    }

    #[test]
    fn descriptors_reuse_channel_provider_classification() {
        let gmail = default_channel_adapters()
            .into_iter()
            .find(|adapter| adapter.provider() == GMAIL_PROVIDER)
            .expect("gmail adapter");
        let descriptor = gmail.descriptor();
        assert_eq!(descriptor.provider, GMAIL_PROVIDER);
        assert_eq!(descriptor.display_label, "Gmail");
        assert_eq!(descriptor.channel, "email");
        assert_eq!(descriptor.channel_label, "email");
    }

    #[test]
    fn capabilities_report_deep_links_only_for_gmail_today() {
        let gmail = capabilities_for(GMAIL_PROVIDER);
        assert!(gmail.pull_ingest);
        assert!(gmail.content_fetch);
        assert!(gmail.deep_link);
        assert!(gmail.connection_status);

        let whatsapp = capabilities_for(WHATSAPP_PROVIDER);
        assert!(whatsapp.pull_ingest);
        assert!(whatsapp.content_fetch);
        assert!(!whatsapp.deep_link);
        assert!(whatsapp.connection_status);
    }

    #[test]
    fn shipped_adapters_do_not_wire_future_realtime_or_extra_capabilities_yet() {
        // iMessage now ships an action adapter (reply → outbound send + draft
        // create); every other shipped adapter still leaves these dormant.
        for adapter in default_channel_adapters() {
            let provider = adapter.provider();
            let capabilities = adapter.capabilities();
            assert!(
                !capabilities.realtime_events,
                "{provider} unexpectedly enables realtime events"
            );
            if provider != IMESSAGE_PROVIDER {
                assert!(
                    !capabilities.outbound_send,
                    "{provider} unexpectedly enables outbound send"
                );
                assert!(
                    !capabilities.draft_create,
                    "{provider} unexpectedly enables draft creation"
                );
                assert!(
                    adapter.action_adapter().is_none(),
                    "{provider} unexpectedly returns an action adapter"
                );
            }
            assert!(
                !capabilities.attachments,
                "{provider} unexpectedly enables attachments"
            );
            assert!(
                !capabilities.reactions,
                "{provider} unexpectedly enables reactions"
            );
            assert!(
                !capabilities.edits_and_deletes,
                "{provider} unexpectedly enables edits/deletes"
            );
            assert!(
                adapter.realtime_adapter().is_none(),
                "{provider} unexpectedly returns a realtime adapter"
            );
        }
    }

    #[test]
    fn imessage_ships_a_reply_action_adapter() {
        let imessage = default_channel_adapters()
            .into_iter()
            .find(|adapter| adapter.provider() == IMESSAGE_PROVIDER)
            .expect("imessage adapter");
        let capabilities = imessage.capabilities();
        assert!(capabilities.outbound_send);
        assert!(capabilities.draft_create);
        let action_adapter = imessage
            .action_adapter()
            .expect("imessage exposes an action adapter");
        let actions = action_adapter.available_actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].id, "reply");
        assert!(actions[0].needs_compose);
        assert!(actions[0].confirm);
    }

    #[test]
    fn channel_identity_keeps_chat_native_ids_without_email_fields() {
        let identity = ChannelIdentity {
            external_conversation_id: "conversation-1".to_string(),
            external_message_id: Some("message-1".to_string()),
            actor_external_id: Some("sender-1".to_string()),
            actor_display_name: Some("Asha".to_string()),
            direction: Some(MessageDirection::Inbound),
        };

        assert_eq!(identity.external_conversation_id, "conversation-1");
        assert_eq!(identity.external_message_id.as_deref(), Some("message-1"));
        assert_eq!(identity.actor_external_id.as_deref(), Some("sender-1"));
        assert_eq!(identity.actor_display_name.as_deref(), Some("Asha"));
        assert_eq!(identity.direction, Some(MessageDirection::Inbound));
    }

    #[test]
    fn gmail_thread_url_preserves_existing_authuser_behavior() {
        let url = thread_url_for(ChannelThreadRef {
            provider: GMAIL_PROVIDER,
            account_alias: "work",
            account_email: Some("owner@example.com"),
            thread_id: "thread-1",
        });
        assert_eq!(
            url.as_deref(),
            Some("https://mail.google.com/mail/?authuser=owner%40example.com#all/thread-1")
        );
    }

    #[test]
    fn gmail_thread_url_fails_closed_without_account_email() {
        let url = thread_url_for(ChannelThreadRef {
            provider: GMAIL_PROVIDER,
            account_alias: "work",
            account_email: None,
            thread_id: "thread-1",
        });
        assert_eq!(url, None);
    }

    #[test]
    fn gmail_thread_url_encodes_plus_address_account_identity() {
        let url = thread_url_for(ChannelThreadRef {
            provider: GMAIL_PROVIDER,
            account_alias: "work",
            account_email: Some("owner+alerts@example.com"),
            thread_id: "thread-1",
        });
        assert_eq!(
            url.as_deref(),
            Some(
                "https://mail.google.com/mail/?authuser=owner%2Balerts%40example.com#all/thread-1"
            )
        );
    }

    #[test]
    fn non_gmail_thread_url_is_absent_until_adapter_supports_it() {
        let url = thread_url_for(ChannelThreadRef {
            provider: AGENTMAIL_PROVIDER,
            account_alias: "work",
            account_email: Some("agent@example.com"),
            thread_id: "thread-1",
        });
        assert_eq!(url, None);
    }
}
