//! Chat-store sink for the unified transport bus.
//!
//! This is the first projection sink in the "one transport bus" migration.
//! Producers emit `RuntimeTransportEvent::ChatMessageReceived` onto the
//! broadcaster; this sink subscribes and projects each event into the
//! `ChatStore` (per-session JSONL on disk). The bus becomes the single
//! place a chat message lands; the chat-store is a downstream
//! consequence, not a parallel write path.
//!
//! Why a sink (vs. inline `chat_store.append_message` from the chat
//! service):
//!   1. **Single rail.** Every emitter publishes ONE event. Persistence,
//!      channel delivery (WhatsApp / Kapso / Telegram), live UI fanout,
//!      and any future projection all subscribe to that same event. No
//!      hand-wired routing between progress router and chat store and
//!      transport log.
//!   2. **Symmetric to outbound channels.** WhatsApp/Kapso adapters
//!      already need to subscribe to chat events to deliver them
//!      externally. Making chat-store a subscriber removes the
//!      "chat-store is special" carve-out.
//!
//! Note on replayability: rebuilding the chat-store from
//! `events.jsonl` is NOT currently safe — `FileChatStore::append_message`
//! does not dedupe by `ChatMessage::id`, so a naive replay of every
//! `ChatMessageReceived` event would duplicate every row. If
//! replayability becomes important later, add id-based dedupe in the
//! store's append path (then this sink can stay as-is).
//!
//! Concurrency model: one spawned tokio task per `ChatStoreSink`. The
//! task owns its `broadcast::Receiver` and processes events serially
//! per session (events for the same session are append-ordered by
//! broadcast delivery). If a single `append_message` is slow, only this
//! sink's queue backs up — other sinks (channel adapters, UI transport)
//! see no contention because they hold their own receivers.

use std::sync::Arc;

use tokio::sync::broadcast::{error::RecvError, Receiver};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::magician_v2::chat::storage::ChatStore;
use crate::magician_v2::progress_channel_seam::{
    ChatChannel as ProgressChatChannel, ExecutionProgressRouter,
};
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

pub struct ChatStoreSink {
    chat_store: Arc<dyn ChatStore>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    /// Used to look up which chat sessions are subscribed to which
    /// execution / task, so the sink can render chat content for the
    /// right sessions when a `ProgressEvent` lands. The router owns
    /// the subscription registry; the sink is just a reader.
    progress_router: ExecutionProgressRouter,
    /// Holds the rendering logic (the big `ProgressMessageKind` match
    /// that builds `ChatMessageContent` from a progress message + its
    /// subscription metadata). Used to be invoked via
    /// `ProgressChannel::deliver` from the router — same code, same
    /// behavior, just driven from the sink now so `ProgressEvent →
    /// ChatStoreSink → chat_store (+ ChatMessageReceived emit)` is one
    /// hop in the sink instead of router→ChatChannel→bus→sink.
    chat_channel: Arc<ProgressChatChannel>,
}

impl ChatStoreSink {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        progress_router: ExecutionProgressRouter,
        chat_channel: Arc<ProgressChatChannel>,
    ) -> Self {
        Self {
            chat_store,
            broadcaster,
            progress_router,
            chat_channel,
        }
    }

    /// Spawn the sink as a background task. The returned `JoinHandle`
    /// is held by the caller for shutdown signalling; dropping it
    /// without aborting lets the task run until the broadcaster's
    /// senders all drop (process exit).
    ///
    /// Subscribes synchronously **before** spawning so any event
    /// emitted between this call returning and the spawned task being
    /// scheduled is buffered in the receiver, not lost. Calling
    /// `broadcast::Sender::subscribe()` from inside the spawned future
    /// leaves a race window where the bus is fully wired but the sink
    /// has no slot in the channel — fast first emits silently vanish.
    pub fn spawn(self) -> JoinHandle<()> {
        let rx = self.broadcaster.subscribe();
        tokio::spawn(async move { self.run(rx).await })
    }

    async fn run(self, mut rx: Receiver<RuntimeTransportEvent>) {
        info!("[chat-store-sink] subscribed to transport bus");
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Err(error) = self.apply(event).await {
                        warn!(
                            error = %error,
                            "[chat-store-sink] failed to project event into chat-store"
                        );
                    }
                },
                Err(RecvError::Lagged(skipped)) => {
                    // Sink fell behind the broadcast buffer. We lost
                    // `skipped` events — log it loudly because chat
                    // messages going missing is a user-visible bug.
                    // The receiver auto-resumes from the head; no
                    // recovery code needed beyond visibility.
                    warn!(
                        skipped,
                        "[chat-store-sink] broadcast lagged — chat messages may be missing from chat_store"
                    );
                },
                Err(RecvError::Closed) => {
                    info!("[chat-store-sink] broadcast closed, exiting");
                    break;
                },
            }
        }
    }

    async fn apply(&self, event: RuntimeTransportEvent) -> anyhow::Result<()> {
        match event {
            // `ChatMessageReceived` — direct chat-store append. Comes
            // from chat-service's `persist_display_message` (user /
            // assistant text), from escalation_listener (HITL cards),
            // and indirectly from the `ProgressEvent` branch below
            // (ChatChannel.deliver renders → emits → we re-enter here
            // to actually write).
            RuntimeTransportEvent::ChatMessageReceived {
                session_id,
                message,
                ..
            } => {
                self.chat_store
                    .append_message(&session_id, message)
                    .await
                    .map_err(|e| anyhow::anyhow!("append_message: {}", e))?;
            },

            // `ProgressEvent` — task/execution lifecycle from the
            // progress projection (artifact_v2, chat-service kickoff,
            // etc.). Previously the router dispatched this through
            // `ChatChannel::deliver`. Now the sink owns the dispatch:
            //   1. Look up chat subscriptions registered for this
            //      message's principal/workspace whose filter matches
            //      (task_id / execution_id / routing_keys).
            //   2. For each match, invoke `ChatChannel::deliver` —
            //      identical rendering, identical emit. The
            //      `ChatMessageReceived` it emits comes right back to
            //      this same sink (above branch), which appends.
            //   3. Subscriptions with `min_severity` higher than the
            //      message's severity are skipped, matching the
            //      router's existing gating semantics.
            //
            // Router skips chat subscriptions on its own dispatch path
            // because `ChatChannel` is no longer `register_channel`'d
            // (see `bin/magician.rs`). The router still routes to
            // `WebhookChannel` (external bot delivery) and any future
            // registered channels — chat is the one carve-out that
            // moved to the sink.
            //
            // Reliability trade-off: the router's `record_delivery_failure`
            // / persisted retry queue / circuit breaker do NOT apply
            // here. A `deliver` failure logs `warn!` and the message
            // is gone — no automatic retry on the next replay sweep.
            // This is acceptable in practice because:
            //   * The vast majority of `deliver` failures are
            //     permanent (session not found, session not active,
            //     missing subscription metadata) — retry wouldn't
            //     help.
            //   * The underlying `ProgressEvent` is still on the bus
            //     and still landed in `events.jsonl` via the
            //     persistence sink, so the activity card / debug
            //     surfaces still see the lifecycle.
            //   * The webhook path (external bots) DOES keep the
            //     router's reliability features — that's the surface
            //     where transient failures (network) are common and
            //     retry pays off.
            // If chat-render retries become important later, copy the
            // `record_delivery_failure` + watermark logic from
            // `progress_channel_seam::router::process_message` into the
            // loop below.
            RuntimeTransportEvent::ProgressEvent { message, .. } => {
                use crate::magician_v2::progress_channel_seam::channel::ProgressChannel;
                let subs = self
                    .progress_router
                    .list_subscriptions(&message.principal, &message.workspace, Some("chat"))
                    .await;
                for subscription in subs {
                    if message.severity < subscription.min_severity {
                        continue;
                    }
                    if !subscription.filter.matches(&message) {
                        continue;
                    }
                    if let Err(error) = self.chat_channel.deliver(&subscription, &message).await {
                        warn!(
                            subscription_id = %subscription.id,
                            session_id = %subscription
                                .metadata
                                .get("session_id")
                                .map(String::as_str)
                                .unwrap_or("(missing)"),
                            error = %error,
                            "[chat-store-sink] chat render failed for ProgressEvent"
                        );
                    }
                }
            },

            // All other variants ignored — sink only cares about chat
            // message persistence and chat-bound progress rendering.
            _ => {},
        }
        Ok(())
    }
}
