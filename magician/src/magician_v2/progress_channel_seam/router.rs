use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    sync::{broadcast, mpsc, RwLock, Semaphore},
    time::{sleep, timeout},
};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::magician_v2::{
    agents::{AgentDefinition, AgentDefinitionStore, CircuitAction},
    artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Service},
    chat::{
        models::{ChatSession, ChatSessionStatus},
        storage::ChatStore,
    },
    orchestrator::v2_orchestrator::MagicianV2Orchestrator,
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

use crate::magician_v2::progress_channel_seam::{
    channel::ProgressChannel,
    event_log::EventLog,
    lineage::ExecutionLineageIndex,
    normalize::{
        apply_subscription_overrides, map_notification_severity, normalize_agent_event,
        normalize_execution_event,
    },
    storage::ProgressChannelStorage,
    types::{
        EventLocator, ProgressMessage, ProgressMessageKind, ProgressSeverity, Subscription,
        SubscriptionFilter, SubscriptionSource,
    },
};

const MAX_SUBSCRIPTIONS: usize = 10_000;
const PENDING_RETRY_CIRCUIT_BREAK_THRESHOLD: usize = 256;
const REPLAY_SWEEP_INTERVAL_SECS: u64 = 60;
/// Cadence of the durability-critical subscription flush.
const INDEX_FLUSH_INTERVAL_MS: u64 = 250;
/// Ticks between slow-lane flushes — 120 × 250 ms = 30 seconds.
///
/// The slow lane carries state whose loss costs nothing but a rebuild or a
/// stale read-only number: the execution lineage cache, and subscription
/// watermarks. Keeping it off the fast lane is what stops a busy run from
/// rewriting two whole maps four times a second.
const SLOW_INDEX_FLUSH_EVERY_TICKS: u32 = 120;
const TERMINAL_DELIVERY_TIMEOUT_SECS: u64 = 10;
const AUTO_AGENT_MEMORY_RULE_NAME: &str = "__agent_memory_progress__";
const AUTO_CIRCUIT_NOTIFY_RULE_NAME: &str = "__circuit_open_notify__";
const CHAT_SUBSCRIPTION_KIND_KEY: &str = "chat_subscription_kind";
const CHAT_SUBSCRIPTION_KIND_LIFECYCLE_RULE: &str = "lifecycle_rule";
const CHAT_RULE_NAME_KEY: &str = "chat_rule_name";
const CHAT_RULE_AGENT_ID_KEY: &str = "chat_rule_agent_id";

#[derive(Clone)]
struct QueuedDelivery {
    subscription: Subscription,
    message: ProgressMessage,
}

#[derive(Clone)]
struct ChannelInfra {
    critical_tx: mpsc::Sender<QueuedDelivery>,
    best_effort_semaphore: Arc<Semaphore>,
}

struct ExecutionProgressRouterInner {
    channels: RwLock<HashMap<String, Arc<dyn ProgressChannel>>>,
    channel_infra: RwLock<HashMap<String, ChannelInfra>>,
    progress_stream: broadcast::Sender<ProgressMessage>,
    subscriptions: Arc<RwLock<HashMap<String, Subscription>>>,
    storage: ProgressChannelStorage,
    lineage_index: ExecutionLineageIndex,
    event_log: EventLog,
    next_seq: AtomicU64,
    /// Set by changes that must survive a restart: pending-retry inserts and
    /// removals, and terminal-subscription cleanup. Flushed every tick.
    subscriptions_dirty: AtomicBool,
    /// Set by a watermark advance, which happens on every matched message.
    ///
    /// A watermark is reported by the subscription-listing API and read by
    /// nothing else — no resume, no replay, no filtering keys off it. Riding
    /// the slow lane trades a possibly stale number in that one response for
    /// not rewriting the whole subscription map four times a second.
    subscription_watermarks_dirty: AtomicBool,
    v3_service: Arc<ArtifactV2Service>,
    orchestrator: Arc<MagicianV2Orchestrator>,
}

#[derive(Clone)]
pub struct ExecutionProgressRouter {
    inner: Arc<ExecutionProgressRouterInner>,
}

impl ExecutionProgressRouter {
    pub async fn new(
        storage_path: impl Into<PathBuf>,
        v3_service: Arc<ArtifactV2Service>,
        orchestrator: Arc<MagicianV2Orchestrator>,
    ) -> anyhow::Result<Self> {
        Self::with_workspace_layout(
            ArtifactV2Workspace::new(storage_path.into()),
            v3_service,
            orchestrator,
        )
        .await
    }

    pub async fn with_workspace_layout(
        workspace_layout: ArtifactV2Workspace,
        v3_service: Arc<ArtifactV2Service>,
        orchestrator: Arc<MagicianV2Orchestrator>,
    ) -> anyhow::Result<Self> {
        let storage =
            ProgressChannelStorage::with_workspace_layout(workspace_layout.clone()).await?;
        let mut subscriptions = storage.load_subscriptions().await?;
        let pruned =
            prune_loaded_subscriptions(&mut subscriptions, &v3_service, &orchestrator).await?;
        if pruned {
            storage.save_subscriptions(&subscriptions).await?;
        }
        let lineage_index = ExecutionLineageIndex::load(storage.clone()).await?;
        let event_log = EventLog::with_workspace_layout(workspace_layout).await?;
        let (progress_stream, _) = broadcast::channel(1024);

        Ok(Self {
            inner: Arc::new(ExecutionProgressRouterInner {
                channels: RwLock::new(HashMap::new()),
                channel_infra: RwLock::new(HashMap::new()),
                progress_stream,
                subscriptions: Arc::new(RwLock::new(subscriptions)),
                storage,
                lineage_index,
                event_log,
                next_seq: AtomicU64::new(1),
                subscriptions_dirty: AtomicBool::new(false),
                subscription_watermarks_dirty: AtomicBool::new(false),
                v3_service,
                orchestrator,
            }),
        })
    }

    pub fn progress_stream(&self) -> broadcast::Receiver<ProgressMessage> {
        self.inner.progress_stream.subscribe()
    }

    pub async fn register_channel(
        &self,
        channel: Arc<dyn ProgressChannel>,
        max_concurrent_deliveries: usize,
    ) {
        let critical_capacity = max_concurrent_deliveries.max(1) * 8;
        let (critical_tx, critical_rx) = mpsc::channel(critical_capacity);
        let channel_id = channel.id().to_string();
        let inner = Arc::clone(&self.inner);
        tokio::spawn(critical_delivery_worker(
            channel.clone(),
            critical_rx,
            inner,
        ));

        self.inner
            .channels
            .write()
            .await
            .insert(channel_id.clone(), channel);
        self.inner.channel_infra.write().await.insert(
            channel_id,
            ChannelInfra {
                critical_tx,
                best_effort_semaphore: Arc::new(Semaphore::new(max_concurrent_deliveries.max(1))),
            },
        );
    }

    pub async fn subscribe(&self, mut subscription: Subscription) -> anyhow::Result<String> {
        let id = if subscription.id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            subscription.id.clone()
        };
        subscription.id = id.clone();
        subscription.created_at = if subscription.created_at == 0 {
            chrono::Utc::now().timestamp_millis()
        } else {
            subscription.created_at
        };

        let mut subscriptions = self.inner.subscriptions.write().await;
        if subscriptions.len() >= MAX_SUBSCRIPTIONS && !subscriptions.contains_key(&id) {
            self.reap_subscriptions_locked(&mut subscriptions).await;
        }
        if subscriptions.len() >= MAX_SUBSCRIPTIONS && !subscriptions.contains_key(&id) {
            anyhow::bail!("subscription capacity reached");
        }
        subscriptions.insert(id.clone(), subscription);
        self.inner
            .storage
            .save_subscriptions(&subscriptions)
            .await?;
        Ok(id)
    }

    pub async fn unsubscribe(&self, subscription_id: &str) -> anyhow::Result<()> {
        let mut subscriptions = self.inner.subscriptions.write().await;
        subscriptions.remove(subscription_id);
        self.inner
            .storage
            .save_subscriptions(&subscriptions)
            .await?;
        Ok(())
    }

    pub async fn get_subscription(&self, subscription_id: &str) -> Option<Subscription> {
        self.inner
            .subscriptions
            .read()
            .await
            .get(subscription_id)
            .cloned()
    }

    pub async fn list_subscriptions(
        &self,
        principal: &str,
        workspace: &str,
        channel_id: Option<&str>,
    ) -> Vec<Subscription> {
        let mut subscriptions = self
            .inner
            .subscriptions
            .read()
            .await
            .values()
            .filter(|subscription| {
                subscription.principal == principal
                    && subscription.workspace == workspace
                    && channel_id
                        .map(|channel_id| subscription.channel_id == channel_id)
                        .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        subscriptions.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        subscriptions
    }

    pub async fn prune_chat_session_lifecycle_subscriptions(
        &self,
        chat_store: Arc<dyn ChatStore>,
    ) -> anyhow::Result<usize> {
        let candidates = self
            .inner
            .subscriptions
            .read()
            .await
            .values()
            .filter(|subscription| is_chat_lifecycle_subscription(subscription))
            .cloned()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(0);
        }

        let mut stale_ids = Vec::new();
        for subscription in candidates {
            let Some(session_id) = subscription.metadata.get("session_id") else {
                stale_ids.push(subscription.id.clone());
                continue;
            };
            let session = chat_store.get_session(session_id).await?;
            let Some(session) = session else {
                stale_ids.push(subscription.id.clone());
                continue;
            };
            let expected_agent_id = subscription
                .metadata
                .get(CHAT_RULE_AGENT_ID_KEY)
                .map(String::as_str)
                .unwrap_or("");
            if session.status != ChatSessionStatus::Active
                || session.principal != subscription.principal
                || session.workspace != subscription.workspace
                || (!expected_agent_id.is_empty() && session.agent_id != expected_agent_id)
            {
                stale_ids.push(subscription.id.clone());
            }
        }

        if stale_ids.is_empty() {
            return Ok(0);
        }
        let mut subscriptions = self.inner.subscriptions.write().await;
        for id in &stale_ids {
            subscriptions.remove(id);
        }
        self.inner
            .storage
            .save_subscriptions(&subscriptions)
            .await?;
        Ok(stale_ids.len())
    }

    pub async fn remove_chat_session_lifecycle_subscriptions(
        &self,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        let mut subscriptions = self.inner.subscriptions.write().await;
        let ids = subscriptions
            .iter()
            .filter_map(|(id, subscription)| {
                if is_chat_lifecycle_subscription(subscription)
                    && subscription
                        .metadata
                        .get("session_id")
                        .map(|value| value == session_id)
                        .unwrap_or(false)
                {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for id in &ids {
            subscriptions.remove(id);
        }
        if !ids.is_empty() {
            self.inner
                .storage
                .save_subscriptions(&subscriptions)
                .await?;
        }
        Ok(ids.len())
    }

    pub async fn sync_chat_session_lifecycle_subscriptions(
        &self,
        session: &ChatSession,
        definition: Option<&AgentDefinition>,
    ) -> anyhow::Result<usize> {
        let mut subscriptions = self.inner.subscriptions.write().await;
        let ids = subscriptions
            .iter()
            .filter_map(|(id, subscription)| {
                if is_chat_lifecycle_subscription(subscription)
                    && subscription
                        .metadata
                        .get("session_id")
                        .map(|value| value == &session.id)
                        .unwrap_or(false)
                {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for id in ids {
            subscriptions.remove(&id);
        }

        let mut created = 0usize;
        if let Some(definition) = definition {
            for (index, rule) in definition.notification_rules.iter().enumerate() {
                if !rule.channels.iter().any(|channel| channel == "chat") {
                    continue;
                }
                let mut metadata = HashMap::new();
                metadata.insert("session_id".to_string(), session.id.clone());
                metadata.insert(
                    CHAT_SUBSCRIPTION_KIND_KEY.to_string(),
                    CHAT_SUBSCRIPTION_KIND_LIFECYCLE_RULE.to_string(),
                );
                metadata.insert(
                    CHAT_RULE_NAME_KEY.to_string(),
                    format!("rule-{}", index + 1),
                );
                metadata.insert(
                    CHAT_RULE_AGENT_ID_KEY.to_string(),
                    definition.agent_id.clone(),
                );
                let subscription = Subscription {
                    id: Uuid::new_v4().to_string(),
                    channel_id: "chat".to_string(),
                    filter: SubscriptionFilter::AgentLifecycleEvent {
                        agent_id: definition.agent_id.clone(),
                        event_pattern: rule.r#match.clone(),
                        condition: rule.condition.clone(),
                    },
                    principal: session.principal.clone(),
                    workspace: session.workspace.clone(),
                    min_severity: ProgressSeverity::Trace,
                    metadata,
                    source: SubscriptionSource::Dynamic,
                    retention_secs: -1,
                    message_template: rule.message.clone(),
                    output_severity: Some(map_notification_severity(rule.severity.clone())),
                    watermark: 0,
                    pending_retry: Default::default(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                subscriptions.insert(subscription.id.clone(), subscription);
                created += 1;
            }

            if circuit_notify_channels(definition)
                .iter()
                .any(|channel| channel == "chat")
                && !has_explicit_lifecycle_rule(definition, "chat", "agent.circuit.opened")
            {
                let mut metadata = HashMap::new();
                metadata.insert("session_id".to_string(), session.id.clone());
                metadata.insert(
                    CHAT_SUBSCRIPTION_KIND_KEY.to_string(),
                    CHAT_SUBSCRIPTION_KIND_LIFECYCLE_RULE.to_string(),
                );
                metadata.insert(
                    CHAT_RULE_NAME_KEY.to_string(),
                    AUTO_CIRCUIT_NOTIFY_RULE_NAME.to_string(),
                );
                metadata.insert(
                    CHAT_RULE_AGENT_ID_KEY.to_string(),
                    definition.agent_id.clone(),
                );
                let subscription = Subscription {
                    id: Uuid::new_v4().to_string(),
                    channel_id: "chat".to_string(),
                    filter: SubscriptionFilter::AgentLifecycleEvent {
                        agent_id: definition.agent_id.clone(),
                        event_pattern: "agent.circuit.opened".to_string(),
                        condition: None,
                    },
                    principal: session.principal.clone(),
                    workspace: session.workspace.clone(),
                    min_severity: ProgressSeverity::Trace,
                    metadata,
                    source: SubscriptionSource::Dynamic,
                    retention_secs: -1,
                    message_template: None,
                    output_severity: None,
                    watermark: 0,
                    pending_retry: Default::default(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                subscriptions.insert(subscription.id.clone(), subscription);
                created += 1;
            }
        }

        self.inner
            .storage
            .save_subscriptions(&subscriptions)
            .await?;
        Ok(created)
    }

    pub async fn subscribe_chat_task(
        &self,
        session_id: &str,
        principal: &str,
        workspace: &str,
        task_id: &str,
        periodic_updates: bool,
    ) -> anyhow::Result<String> {
        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session_id.to_string());
        metadata.insert("periodic_updates".to_string(), periodic_updates.to_string());
        self.subscribe(Subscription {
            id: String::new(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::TaskId(task_id.to_string()),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        })
        .await
    }

    pub async fn subscribe_chat_execution(
        &self,
        session_id: &str,
        principal: &str,
        workspace: &str,
        execution_id: &str,
        periodic_updates: bool,
    ) -> anyhow::Result<String> {
        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session_id.to_string());
        metadata.insert("periodic_updates".to_string(), periodic_updates.to_string());
        self.subscribe(Subscription {
            id: String::new(),
            channel_id: "chat".to_string(),
            filter: SubscriptionFilter::ExecutionId(execution_id.to_string()),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            min_severity: ProgressSeverity::Trace,
            metadata,
            source: SubscriptionSource::Dynamic,
            retention_secs: 0,
            message_template: None,
            output_severity: None,
            watermark: 0,
            pending_retry: Default::default(),
            created_at: 0,
        })
        .await
    }

    pub async fn replay(&self, subscription_id: &str) -> anyhow::Result<usize> {
        let subscription = {
            self.inner
                .subscriptions
                .read()
                .await
                .get(subscription_id)
                .cloned()
        };
        let Some(subscription) = subscription else {
            return Ok(0);
        };

        let channel = {
            self.inner
                .channels
                .read()
                .await
                .get(&subscription.channel_id)
                .cloned()
        };
        let Some(channel) = channel else {
            return Ok(0);
        };

        let pending = subscription
            .pending_retry
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut replayed = 0usize;
        for locator in pending {
            let Some(message) = self.inner.event_log.load(&locator).await? else {
                continue;
            };
            let delivered_message = apply_subscription_overrides(&subscription, message.clone());
            match channel.deliver(&subscription, &delivered_message).await {
                Ok(_) => {
                    self.clear_pending_retry(subscription_id, &locator).await?;
                    replayed += 1;
                },
                Err(error) => {
                    debug!(
                        subscription_id,
                        channel = subscription.channel_id,
                        seq = locator.seq,
                        error = %error,
                        "progress replay delivery failed"
                    );
                },
            }
        }

        Ok(replayed)
    }

    pub async fn reconcile_declarative_subscriptions(
        &self,
        definition_store: &AgentDefinitionStore,
    ) -> anyhow::Result<()> {
        let definitions = definition_store
            .list_all_definitions_across_scopes()
            .await?;
        let active_channels = self
            .inner
            .channels
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let desired = build_declarative_subscriptions(
            &definitions
                .into_iter()
                .map(|record| record.definition)
                .collect::<Vec<_>>(),
            &active_channels,
        );

        let mut subscriptions = self.inner.subscriptions.write().await;
        subscriptions.retain(|_, subscription| match &subscription.source {
            SubscriptionSource::Declarative {
                agent_id,
                rule_name,
            } => desired.contains_key(&(
                subscription.principal.clone(),
                subscription.workspace.clone(),
                agent_id.clone(),
                rule_name.clone(),
                subscription.channel_id.clone(),
            )),
            SubscriptionSource::Dynamic => true,
        });

        for ((principal, workspace, agent_id, rule_name, channel_id), desired_subscription) in
            desired
        {
            let existing = subscriptions.values_mut().find(|subscription| {
                matches!(
                    &subscription.source,
                    SubscriptionSource::Declarative {
                        agent_id: existing_agent_id,
                        rule_name: existing_rule_name
                    } if existing_agent_id == &agent_id && existing_rule_name == &rule_name
                ) && subscription.principal == principal
                    && subscription.workspace == workspace
                    && subscription.channel_id == channel_id
            });

            if let Some(existing) = existing {
                existing.filter = desired_subscription.filter;
                existing.min_severity = desired_subscription.min_severity;
                existing.message_template = desired_subscription.message_template;
                existing.output_severity = desired_subscription.output_severity;
                existing.metadata = desired_subscription.metadata;
            } else {
                subscriptions.insert(desired_subscription.id.clone(), desired_subscription);
            }
        }

        self.inner
            .storage
            .save_subscriptions(&subscriptions)
            .await?;
        Ok(())
    }

    pub fn start(&self, event_broadcaster: Arc<RuntimeTransportBroadcaster>) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let mut receiver = event_broadcaster.subscribe();
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if let Err(error) = process_event(&inner, event).await {
                            warn!(error = %error, "progress router failed to process event");
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "progress router lagged on realtime events");
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let router = self.clone();
        tokio::spawn(async move {
            loop {
                sleep(Duration::from_secs(REPLAY_SWEEP_INTERVAL_SECS)).await;
                if let Err(error) = router.replay_pending_subscriptions().await {
                    warn!(error = %error, "progress router replay sweep failed");
                }
            }
        });

        // One flush loop, two lanes.
        //
        // Fast lane (every tick): subscription state whose loss would change
        // behaviour after a restart — a pending retry that must still be
        // replayed, a terminal subscription that must stay removed.
        //
        // Slow lane (every `SLOW_INDEX_FLUSH_EVERY_TICKS`): the two indexes
        // that are rebuildable or purely informational. Both are whole-map
        // rewrites — `O(N)` clone, `O(N)` serialize, two disk barriers — so
        // what matters is how often they run, not how fast each one is.
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(INDEX_FLUSH_INTERVAL_MS));
            let mut ticks_since_slow_flush = 0_u32;
            loop {
                interval.tick().await;

                ticks_since_slow_flush += 1;
                let slow_lane_due = ticks_since_slow_flush >= SLOW_INDEX_FLUSH_EVERY_TICKS;
                if slow_lane_due {
                    ticks_since_slow_flush = 0;
                }

                let critical_due = inner.subscriptions_dirty.swap(false, Ordering::SeqCst);
                let watermarks_due = slow_lane_due
                    && inner
                        .subscription_watermarks_dirty
                        .swap(false, Ordering::SeqCst);
                if critical_due || watermarks_due {
                    // One write persists both kinds of change, so a fast-lane
                    // flush also settles any watermark waiting on the slow lane.
                    inner
                        .subscription_watermarks_dirty
                        .store(false, Ordering::SeqCst);
                    let snapshot = inner.subscriptions.read().await.clone();
                    if let Err(error) = inner.storage.save_subscriptions(&snapshot).await {
                        warn!(error = %error, "progress router failed to flush subscriptions");
                        inner.subscriptions_dirty.store(true, Ordering::SeqCst);
                    }
                }

                if slow_lane_due {
                    if let Err(error) = inner.lineage_index.flush_if_dirty().await {
                        warn!(error = %error, "progress router failed to flush execution lineage");
                    }
                }
            }
        });
    }

    async fn replay_pending_subscriptions(&self) -> anyhow::Result<()> {
        let ids = self
            .inner
            .subscriptions
            .read()
            .await
            .iter()
            .filter_map(|(id, subscription)| {
                if subscription.pending_retry.is_empty() {
                    None
                } else {
                    Some(id.clone())
                }
            })
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.replay(&id).await?;
        }
        Ok(())
    }

    async fn record_delivery_failure(
        &self,
        subscription_id: &str,
        message: &ProgressMessage,
    ) -> anyhow::Result<()> {
        // Replay loads the message back out of its shard by locator. An
        // ephemeral message was never written there, so a pending retry for it
        // could only ever fail to resolve — and would sit in the set forever,
        // re-read on every 60-second sweep, counting towards the circuit
        // breaker. A missed liveness ping is superseded by the next one.
        if message.is_ephemeral() {
            return Ok(());
        }
        let locator = EventLocator {
            principal: message.principal.clone(),
            workspace: message.workspace.clone(),
            log_key: message.log_key.clone(),
            seq: message.seq,
            message_id: Some(message.id.clone()),
        };
        let mut subscriptions = self.inner.subscriptions.write().await;
        if let Some(subscription) = subscriptions.get_mut(subscription_id) {
            subscription.pending_retry.insert(locator);
        }
        self.inner.subscriptions_dirty.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn clear_pending_retry(
        &self,
        subscription_id: &str,
        locator: &EventLocator,
    ) -> anyhow::Result<()> {
        let mut subscriptions = self.inner.subscriptions.write().await;
        if let Some(subscription) = subscriptions.get_mut(subscription_id) {
            subscription.pending_retry.remove(locator);
        }
        self.inner.subscriptions_dirty.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn reap_subscriptions_locked(&self, subscriptions: &mut HashMap<String, Subscription>) {
        let now_secs = chrono::Utc::now().timestamp();
        let ids = subscriptions
            .iter()
            .filter_map(|(id, subscription)| {
                let expired = subscription.retention_secs > 0
                    && subscription.created_at / 1000 + subscription.retention_secs <= now_secs;
                if expired {
                    return Some(id.clone());
                }
                None
            })
            .collect::<Vec<_>>();
        for id in ids {
            subscriptions.remove(&id);
        }
    }
}

async fn prune_loaded_subscriptions(
    subscriptions: &mut HashMap<String, Subscription>,
    v3_service: &Arc<ArtifactV2Service>,
    orchestrator: &Arc<MagicianV2Orchestrator>,
) -> anyhow::Result<bool> {
    let now_secs = chrono::Utc::now().timestamp();
    let ids = subscriptions
        .iter()
        .filter_map(|(id, subscription)| {
            let expired = subscription.retention_secs > 0
                && subscription.created_at / 1000 + subscription.retention_secs <= now_secs;
            if expired {
                return Some(id.clone());
            }
            None
        })
        .collect::<Vec<_>>();
    let mut removed_any = !ids.is_empty();
    for id in ids {
        subscriptions.remove(&id);
    }

    let terminal_candidates = subscriptions
        .iter()
        .filter_map(|(id, subscription)| {
            if subscription.retention_secs == 0 {
                Some((id.clone(), subscription.filter.clone()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    for (id, filter) in terminal_candidates {
        let terminal = match filter {
            SubscriptionFilter::TaskId(task_id) => v3_service
                .get_task_by_id(&task_id)
                .await
                .ok()
                .flatten()
                .map(|(_, task)| {
                    matches!(
                        task.state.status.as_str(),
                        "completed" | "failed" | "cancelled"
                    )
                })
                .unwrap_or(false),
            SubscriptionFilter::ExecutionId(execution_id) => orchestrator
                .get_execution(&execution_id)
                .await
                .map(|execution| execution.waiting_state.is_terminal())
                .unwrap_or(false),
            _ => false,
        };
        if terminal && subscriptions.remove(&id).is_some() {
            removed_any = true;
        }
    }

    Ok(removed_any)
}

async fn critical_delivery_worker(
    channel: Arc<dyn ProgressChannel>,
    mut receiver: mpsc::Receiver<QueuedDelivery>,
    inner: Arc<ExecutionProgressRouterInner>,
) {
    while let Some(delivery) = receiver.recv().await {
        let delivered_message =
            apply_subscription_overrides(&delivery.subscription, delivery.message.clone());
        let result = channel
            .deliver(&delivery.subscription, &delivered_message)
            .await;
        if let Err(error) = result {
            warn!(
                channel = channel.id(),
                subscription_id = delivery.subscription.id,
                error = %error,
                "critical progress delivery failed; retrying once"
            );
            sleep(Duration::from_secs(1)).await;
            if let Err(retry_error) = channel
                .deliver(&delivery.subscription, &delivered_message)
                .await
            {
                warn!(
                    channel = channel.id(),
                    subscription_id = delivery.subscription.id,
                    error = %retry_error,
                    "critical progress delivery retry failed"
                );
                let router = ExecutionProgressRouter {
                    inner: Arc::clone(&inner),
                };
                let _ = router
                    .record_delivery_failure(&delivery.subscription.id, &delivery.message)
                    .await;
            }
        }
    }
}

async fn process_event(
    inner: &Arc<ExecutionProgressRouterInner>,
    event: RuntimeTransportEvent,
) -> anyhow::Result<()> {
    let normalized = match &event {
        RuntimeTransportEvent::AgentEvent { event } => {
            normalize_agent_event(
                event,
                &inner.lineage_index,
                &inner.v3_service,
                &inner.orchestrator,
            )
            .await
        },
        // `ProgressEvent` is the bus envelope for a fully-formed
        // `ProgressMessage` — used by producers that previously called
        // `ProgressRouter::publish_message` directly. Unwrap the payload
        // and feed it straight to `process_message`; no normalization
        // needed because the producer already shaped the message.
        // Single-rail invariant: every producer emits to the bus; the
        // router is a bus consumer, never a parallel input path.
        RuntimeTransportEvent::ProgressEvent { message, .. } => {
            return process_message(inner, message.clone()).await;
        },
        _ => {
            normalize_execution_event(
                &event,
                &inner.lineage_index,
                &inner.v3_service,
                &inner.orchestrator,
            )
            .await
        },
    };
    let Some(message) = normalized else {
        return Ok(());
    };

    process_message(inner, message).await
}

async fn process_message(
    inner: &Arc<ExecutionProgressRouterInner>,
    mut message: ProgressMessage,
) -> anyhow::Result<()> {
    message.seq = inner.next_seq.fetch_add(1, Ordering::SeqCst);
    inner.lineage_index.update_from_message(&message).await?;
    // An ephemeral message is broadcast and delivered like any other; it just
    // never lands in the shard. `EventLog::append` is a durable append —
    // create_dir_all, open, write, flush, fsync, close — and a liveness ping
    // has no reader that would ever load it back.
    if !message.is_ephemeral() {
        inner.event_log.append(&message).await?;
    }
    let _ = inner.progress_stream.send(message.clone());

    let matching_subscriptions = {
        let mut subscriptions = inner.subscriptions.write().await;
        let mut matched = Vec::new();
        for subscription in subscriptions.values_mut() {
            if subscription.principal != message.principal
                || subscription.workspace != message.workspace
            {
                continue;
            }
            if message.severity < subscription.min_severity {
                continue;
            }
            if !subscription.filter.matches(&message) {
                continue;
            }
            subscription.watermark = message.seq;
            matched.push(subscription.clone());
        }
        if !matched.is_empty() {
            // Only the watermark moved. That rides the slow lane; the fast
            // lane is reserved for changes a restart would actually miss.
            inner
                .subscription_watermarks_dirty
                .store(true, Ordering::SeqCst);
        }
        matched
    };

    for subscription in matching_subscriptions {
        let channels = inner.channels.read().await;
        let Some(channel) = channels.get(&subscription.channel_id).cloned() else {
            continue;
        };
        drop(channels);
        let infra = { inner.channel_infra.read().await.get(channel.id()).cloned() };
        let Some(infra) = infra else {
            continue;
        };

        let circuit_broken =
            subscription.pending_retry.len() > PENDING_RETRY_CIRCUIT_BREAK_THRESHOLD;
        if circuit_broken {
            let router = ExecutionProgressRouter {
                inner: Arc::clone(inner),
            };
            router
                .record_delivery_failure(&subscription.id, &message)
                .await?;
            continue;
        }

        let terminal = message.is_terminal();
        let critical = message.severity >= ProgressSeverity::Warning || terminal;

        if terminal {
            let delivered_message = apply_subscription_overrides(&subscription, message.clone());
            if let Err(error) =
                deliver_terminal_with_timeout(channel.as_ref(), &subscription, &delivered_message)
                    .await
            {
                warn!(
                    subscription_id = subscription.id,
                    channel = subscription.channel_id,
                    error = %error,
                    "terminal progress delivery failed; retrying once"
                );
                sleep(Duration::from_secs(1)).await;
                if let Err(retry_error) = deliver_terminal_with_timeout(
                    channel.as_ref(),
                    &subscription,
                    &delivered_message,
                )
                .await
                {
                    warn!(
                        subscription_id = subscription.id,
                        channel = subscription.channel_id,
                        error = %retry_error,
                        "terminal progress delivery retry failed"
                    );
                    let router = ExecutionProgressRouter {
                        inner: Arc::clone(inner),
                    };
                    router
                        .record_delivery_failure(&subscription.id, &message)
                        .await?;
                }
            }
        } else if critical {
            if let Err(error) = infra.critical_tx.try_send(QueuedDelivery {
                subscription: subscription.clone(),
                message: message.clone(),
            }) {
                warn!(
                    subscription_id = subscription.id,
                    channel = subscription.channel_id,
                    error = %error,
                    "critical delivery queue full; persisting for replay"
                );
                let router = ExecutionProgressRouter {
                    inner: Arc::clone(inner),
                };
                router
                    .record_delivery_failure(&subscription.id, &message)
                    .await?;
            }
        } else {
            match infra.best_effort_semaphore.clone().try_acquire_owned() {
                Ok(permit) => {
                    let router = ExecutionProgressRouter {
                        inner: Arc::clone(inner),
                    };
                    let subscription = subscription.clone();
                    let message = message.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let delivered_message =
                            apply_subscription_overrides(&subscription, message.clone());
                        if let Err(error) = channel.deliver(&subscription, &delivered_message).await
                        {
                            debug!(
                                subscription_id = subscription.id,
                                channel = subscription.channel_id,
                                error = %error,
                                "best-effort progress delivery failed"
                            );
                            let _ = router
                                .record_delivery_failure(&subscription.id, &message)
                                .await;
                        }
                    });
                },
                Err(_) => {
                    debug!(
                        channel = subscription.channel_id,
                        seq = message.seq,
                        "shedding best-effort progress event because channel is saturated"
                    );
                },
            }
        }
    }

    if matches!(
        &message.kind,
        ProgressMessageKind::StatusChanged { status, .. }
            if matches!(status.as_str(), "completed" | "failed" | "cancelled")
    ) {
        cleanup_terminal_subscriptions(inner, &message).await?;
    }

    Ok(())
}

async fn deliver_terminal_with_timeout(
    channel: &dyn ProgressChannel,
    subscription: &Subscription,
    message: &ProgressMessage,
) -> anyhow::Result<()> {
    timeout(
        Duration::from_secs(TERMINAL_DELIVERY_TIMEOUT_SECS),
        channel.deliver(subscription, message),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "terminal progress delivery timed out after {}s",
            TERMINAL_DELIVERY_TIMEOUT_SECS
        )
    })?
}

async fn cleanup_terminal_subscriptions(
    inner: &Arc<ExecutionProgressRouterInner>,
    message: &ProgressMessage,
) -> anyhow::Result<()> {
    let mut subscriptions = inner.subscriptions.write().await;
    let to_remove = subscriptions
        .iter()
        .filter_map(|(id, subscription)| {
            if subscription.retention_secs != 0 {
                return None;
            }
            let matches = match &subscription.filter {
                SubscriptionFilter::TaskId(task_id) => {
                    message.root_task_id.as_deref() == Some(task_id.as_str())
                        || message.task_id.as_deref() == Some(task_id.as_str())
                },
                SubscriptionFilter::ExecutionId(execution_id) => {
                    message.execution_id.as_deref() == Some(execution_id.as_str())
                        || message.root_execution_id.as_deref() == Some(execution_id.as_str())
                },
                _ => false,
            };
            if matches {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    let removed_any = !to_remove.is_empty();
    for id in to_remove {
        subscriptions.remove(&id);
    }
    if removed_any {
        inner.subscriptions_dirty.store(true, Ordering::SeqCst);
    }
    Ok(())
}

pub fn build_declarative_subscriptions(
    definitions: &[AgentDefinition],
    active_channels: &HashSet<String>,
) -> HashMap<(String, String, String, String, String), Subscription> {
    let mut desired = HashMap::new();
    let default_webhook_metadata = declarative_webhook_metadata();
    for definition in definitions {
        let Some((principal, workspace)) = declarative_subscription_scope(definition) else {
            warn!(
                agent_id = %definition.agent_id,
                "skipping declarative subscriptions because the agent definition is missing explicit scope"
            );
            continue;
        };
        for (index, rule) in definition.notification_rules.iter().enumerate() {
            for channel_id in &rule.channels {
                if !active_channels.contains(channel_id) {
                    continue;
                }
                if channel_id == "chat" {
                    // Chat lifecycle rules remain session-scoped. The current phase
                    // only wires task/execution-scoped dynamic chat subscriptions.
                    continue;
                }
                let rule_name = format!("rule-{}", index + 1);
                let key = (
                    principal.clone(),
                    workspace.clone(),
                    definition.agent_id.clone(),
                    rule_name.clone(),
                    channel_id.clone(),
                );
                let metadata = match channel_id.as_str() {
                    "webhook" => {
                        let Some(metadata) = default_webhook_metadata.clone() else {
                            warn!(
                                agent_id = %definition.agent_id,
                                rule_name,
                                "skipping declarative webhook subscription because no default webhook target is configured"
                            );
                            continue;
                        };
                        metadata
                    },
                    "agent_memory" => agent_memory_metadata(&definition.agent_id, "episode"),
                    _ => HashMap::new(),
                };
                let subscription = Subscription {
                    id: Uuid::new_v4().to_string(),
                    channel_id: channel_id.clone(),
                    filter: SubscriptionFilter::AgentLifecycleEvent {
                        agent_id: definition.agent_id.clone(),
                        event_pattern: rule.r#match.clone(),
                        condition: rule.condition.clone(),
                    },
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    min_severity: ProgressSeverity::Trace,
                    metadata,
                    source: SubscriptionSource::Declarative {
                        agent_id: definition.agent_id.clone(),
                        rule_name,
                    },
                    retention_secs: -1,
                    message_template: rule.message.clone(),
                    output_severity: Some(map_notification_severity(rule.severity.clone())),
                    watermark: 0,
                    pending_retry: Default::default(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                desired.insert(key, subscription);
            }
        }

        for channel_id in circuit_notify_channels(definition) {
            if !active_channels.contains(&channel_id) {
                continue;
            }
            if channel_id == "chat" {
                continue;
            }
            if has_explicit_lifecycle_rule(definition, &channel_id, "agent.circuit.opened") {
                continue;
            }
            let key = (
                principal.clone(),
                workspace.clone(),
                definition.agent_id.clone(),
                AUTO_CIRCUIT_NOTIFY_RULE_NAME.to_string(),
                channel_id.clone(),
            );
            let metadata = match channel_id.as_str() {
                "webhook" => {
                    let Some(metadata) = default_webhook_metadata.clone() else {
                        warn!(
                            agent_id = %definition.agent_id,
                            "skipping declarative webhook circuit notify because no default webhook target is configured"
                        );
                        continue;
                    };
                    metadata
                },
                "agent_memory" => agent_memory_metadata(&definition.agent_id, "episode"),
                _ => HashMap::new(),
            };
            let subscription = Subscription {
                id: Uuid::new_v4().to_string(),
                channel_id: channel_id.clone(),
                filter: SubscriptionFilter::AgentLifecycleEvent {
                    agent_id: definition.agent_id.clone(),
                    event_pattern: "agent.circuit.opened".to_string(),
                    condition: None,
                },
                principal: principal.clone(),
                workspace: workspace.clone(),
                min_severity: ProgressSeverity::Trace,
                metadata,
                source: SubscriptionSource::Declarative {
                    agent_id: definition.agent_id.clone(),
                    rule_name: AUTO_CIRCUIT_NOTIFY_RULE_NAME.to_string(),
                },
                retention_secs: -1,
                message_template: None,
                output_severity: None,
                watermark: 0,
                pending_retry: Default::default(),
                created_at: chrono::Utc::now().timestamp_millis(),
            };
            desired.insert(key, subscription);
        }

        if active_channels.contains("agent_memory") && !definition.memory_tiers.is_empty() {
            let key = (
                principal.clone(),
                workspace.clone(),
                definition.agent_id.clone(),
                AUTO_AGENT_MEMORY_RULE_NAME.to_string(),
                "agent_memory".to_string(),
            );
            desired.insert(
                key,
                Subscription {
                    id: Uuid::new_v4().to_string(),
                    channel_id: "agent_memory".to_string(),
                    filter: SubscriptionFilter::AgentId(definition.agent_id.clone()),
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    min_severity: ProgressSeverity::Trace,
                    metadata: agent_memory_metadata(&definition.agent_id, "episode"),
                    source: SubscriptionSource::Declarative {
                        agent_id: definition.agent_id.clone(),
                        rule_name: AUTO_AGENT_MEMORY_RULE_NAME.to_string(),
                    },
                    retention_secs: -1,
                    message_template: None,
                    output_severity: None,
                    watermark: 0,
                    pending_retry: Default::default(),
                    created_at: chrono::Utc::now().timestamp_millis(),
                },
            );
        }
    }
    desired
}

fn declarative_subscription_scope(definition: &AgentDefinition) -> Option<(String, String)> {
    let principal = definition
        .principal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    let workspace = definition
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    Some((principal, workspace))
}

fn circuit_notify_channels(definition: &AgentDefinition) -> Vec<String> {
    let mut channels = Vec::new();
    let mut seen = HashSet::new();
    let Some(policy) = definition.circuit_breaker.as_ref() else {
        return channels;
    };
    for threshold in &policy.thresholds {
        if !matches!(threshold.action, CircuitAction::OpenCircuit) {
            continue;
        }
        for channel in &threshold.notify {
            if seen.insert(channel.clone()) {
                channels.push(channel.clone());
            }
        }
    }
    channels
}

fn has_explicit_lifecycle_rule(
    definition: &AgentDefinition,
    channel_id: &str,
    event_pattern: &str,
) -> bool {
    definition.notification_rules.iter().any(|rule| {
        event_pattern.starts_with(rule.r#match.as_str())
            && rule.channels.iter().any(|channel| channel == channel_id)
    })
}

fn is_chat_lifecycle_subscription(subscription: &Subscription) -> bool {
    subscription.channel_id == "chat"
        && subscription
            .metadata
            .get(CHAT_SUBSCRIPTION_KIND_KEY)
            .map(|value| value == CHAT_SUBSCRIPTION_KIND_LIFECYCLE_RULE)
            .unwrap_or(false)
}

fn declarative_webhook_metadata() -> Option<HashMap<String, String>> {
    let url = std::env::var("MAGICIAN_PROGRESS_WEBHOOK_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("SLACK_WEBHOOK_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })?;
    let mut metadata = HashMap::new();
    metadata.insert("url".to_string(), url);
    if let Ok(headers) = std::env::var("MAGICIAN_PROGRESS_WEBHOOK_HEADERS") {
        if !headers.trim().is_empty() {
            metadata.insert("headers".to_string(), headers);
        }
    }
    Some(metadata)
}

fn agent_memory_metadata(agent_id: &str, tier_name: &str) -> HashMap<String, String> {
    HashMap::from([
        ("agent_id".to_string(), agent_id.to_string()),
        ("tier_name".to_string(), tier_name.to_string()),
    ])
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        build_declarative_subscriptions, process_message, AUTO_AGENT_MEMORY_RULE_NAME,
        AUTO_CIRCUIT_NOTIFY_RULE_NAME,
    };
    use crate::magician_v2::agents::CircuitAction;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use uuid::Uuid;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use std::collections::HashSet;

    use crate::magician_v2::agents::{
        AgentDefinition, MemoryTierDefinition, NotificationRule, NotificationSeverity,
        RenderConfig, RetentionMode, TierScope,
    };

    use crate::magician_v2::progress_channel_seam::*;

    // `terminal_progress_delivery_blocks_until_channel_finishes` was
    // removed alongside `ProgressRouter::publish_message`. The
    // synchronous "publish blocks until terminal channel finishes"
    // contract was a property of the direct `process_message` path —
    // not part of the single-rail bus model. Producers now call
    // `RuntimeTransportBroadcaster::emit_progress` (fire-and-forget);
    // reliable terminal delivery is enforced inside `process_message`
    // for the channels the router dispatches to, but no caller waits
    // on that.

    fn test_agent_definition(agent_id: &str) -> AgentDefinition {
        let mut definition: AgentDefinition = serde_yaml::from_str(&format!(
            r#"
agent_id: "{agent_id}"
name: "Test Agent"
description: "Test agent"
persona: "Test persona"
kind: personal
"#
        ))
        .expect("minimal test agent definition");
        definition.principal = Some("principal-a".to_string());
        definition.workspace = Some("workspace-a".to_string());
        definition.memory_tiers = vec![MemoryTierDefinition {
            name: "episode".to_string(),
            scope: TierScope::Agent,
            description: "Episode memory".to_string(),
            schema: Default::default(),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{summary}".to_string(),
            },
            retention: RetentionMode::Forever,
        }];
        definition
    }

    #[test]
    fn build_declarative_subscriptions_adds_auto_agent_memory_subscription() {
        let definition = test_agent_definition("agent-a");
        let active_channels = HashSet::from(["agent_memory".to_string()]);

        let desired = build_declarative_subscriptions(&[definition], &active_channels);

        let subscription = desired
            .get(&(
                "principal-a".to_string(),
                "workspace-a".to_string(),
                "agent-a".to_string(),
                AUTO_AGENT_MEMORY_RULE_NAME.to_string(),
                "agent_memory".to_string(),
            ))
            .expect("auto agent_memory subscription");
        assert_eq!(subscription.channel_id, "agent_memory");
        assert_eq!(
            subscription.filter,
            SubscriptionFilter::AgentId("agent-a".to_string())
        );
        assert_eq!(
            subscription.metadata.get("tier_name").map(String::as_str),
            Some("episode")
        );
    }

    #[test]
    fn build_declarative_subscriptions_resolves_webhook_metadata_from_env() {
        std::env::set_var(
            "MAGICIAN_PROGRESS_WEBHOOK_URL",
            "https://example.test/progress",
        );
        std::env::set_var(
            "MAGICIAN_PROGRESS_WEBHOOK_HEADERS",
            r#"{"x-progress-token":"secret"}"#,
        );

        let mut definition = test_agent_definition("agent-a");
        definition.notification_rules.push(NotificationRule {
            r#match: "agent.cycle.failed".to_string(),
            severity: NotificationSeverity::High,
            channels: vec!["webhook".to_string()],
            condition: None,
            message: Some("cycle failed".to_string()),
        });
        let active_channels = HashSet::from(["webhook".to_string()]);

        let desired = build_declarative_subscriptions(&[definition], &active_channels);

        let subscription = desired
            .values()
            .find(|subscription| subscription.channel_id == "webhook")
            .expect("webhook subscription");
        assert_eq!(
            subscription.metadata.get("url").map(String::as_str),
            Some("https://example.test/progress")
        );
        assert_eq!(
            subscription.metadata.get("headers").map(String::as_str),
            Some(r#"{"x-progress-token":"secret"}"#)
        );

        std::env::remove_var("MAGICIAN_PROGRESS_WEBHOOK_URL");
        std::env::remove_var("MAGICIAN_PROGRESS_WEBHOOK_HEADERS");
    }

    #[test]
    fn build_declarative_subscriptions_materializes_circuit_notify_targets() {
        let mut definition = test_agent_definition("agent-a");
        definition.notification_rules.clear();
        definition.circuit_breaker = Some(crate::magician_v2::agents::CircuitBreakerPolicy {
            thresholds: vec![crate::magician_v2::agents::CircuitBreakerThreshold {
                failures: 3,
                action: CircuitAction::OpenCircuit,
                escalation: None,
                notify: vec!["agent_memory".to_string()],
            }],
            ..Default::default()
        });
        let active_channels = HashSet::from(["agent_memory".to_string()]);

        let desired = build_declarative_subscriptions(&[definition], &active_channels);

        let subscription = desired
            .get(&(
                "principal-a".to_string(),
                "workspace-a".to_string(),
                "agent-a".to_string(),
                AUTO_CIRCUIT_NOTIFY_RULE_NAME.to_string(),
                "agent_memory".to_string(),
            ))
            .expect("circuit notify subscription");
        assert_eq!(subscription.channel_id, "agent_memory");
        assert_eq!(
            subscription.filter,
            SubscriptionFilter::AgentLifecycleEvent {
                agent_id: "agent-a".to_string(),
                event_pattern: "agent.circuit.opened".to_string(),
                condition: None,
            }
        );
        assert!(subscription.output_severity.is_none());
    }

    #[test]
    fn build_declarative_subscriptions_dedupes_explicit_circuit_prefix_rules() {
        let mut definition = test_agent_definition("agent-a");
        definition.notification_rules = vec![NotificationRule {
            r#match: "agent.circuit".to_string(),
            severity: NotificationSeverity::High,
            channels: vec!["agent_memory".to_string()],
            condition: None,
            message: Some("Circuit opened".to_string()),
        }];
        definition.circuit_breaker = Some(crate::magician_v2::agents::CircuitBreakerPolicy {
            thresholds: vec![crate::magician_v2::agents::CircuitBreakerThreshold {
                failures: 3,
                action: CircuitAction::OpenCircuit,
                escalation: None,
                notify: vec!["agent_memory".to_string()],
            }],
            ..Default::default()
        });
        let active_channels = HashSet::from(["agent_memory".to_string()]);

        let desired = build_declarative_subscriptions(&[definition], &active_channels);

        assert_eq!(
            desired
                .values()
                .filter(|subscription| {
                    subscription.channel_id == "agent_memory"
                        && matches!(
                            subscription.filter,
                            SubscriptionFilter::AgentLifecycleEvent { ref event_pattern, .. }
                                if event_pattern.starts_with("agent.circuit")
                        )
                })
                .count(),
            1
        );
    }

    #[test]
    fn build_declarative_subscriptions_skips_missing_workspace() {
        let mut definition = test_agent_definition("agent-a");
        definition.workspace = None;
        let active_channels = HashSet::from(["agent_memory".to_string()]);

        let desired = build_declarative_subscriptions(&[definition], &active_channels);

        assert!(desired.is_empty());
    }

    // ── Write-amplification contracts ────────────────────────────────
    //
    // These pin the three costs that made the router the hottest fsync
    // source in the process: a lineage write per message, a subscription
    // write per matched message, and a durable append per liveness ping.

    use crate::magician_v2::{
        media_seam::{MEDIA_SESSION_HEARTBEAT, MEDIA_SESSION_REGISTERED},
        progress_channel_seam::types::{ExecutionLineage, ProgressSource},
    };

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    async fn test_router(root: &std::path::Path) -> (ExecutionProgressRouter, ArtifactV2Workspace) {
        let (service, orchestrator) =
            crate::magician_v2::test_support::build_test_artifact_v2_harness(root);
        let workspace_layout = ArtifactV2Workspace::new(root.join("progress"));
        let router = ExecutionProgressRouter::with_workspace_layout(
            workspace_layout.clone(),
            service,
            orchestrator,
        )
        .await
        .expect("progress router");
        (router, workspace_layout)
    }

    fn base_test_message(kind: ProgressMessageKind, log_key: &str) -> ProgressMessage {
        ProgressMessage {
            id: Uuid::new_v4().to_string(),
            seq: 0,
            log_key: log_key.to_string(),
            source: ProgressSource::AgentLifecycle,
            event_type: None,
            metadata: Default::default(),
            execution_id: None,
            task_id: None,
            root_task_id: None,
            root_execution_id: None,
            parent_execution_id: None,
            agent_id: None,
            ui_thread_id: None,
            step_id: None,
            routing_keys: Vec::new(),
            principal: PRINCIPAL.to_string(),
            workspace: WORKSPACE.to_string(),
            severity: ProgressSeverity::Info,
            kind,
            timestamp: 0,
        }
    }

    fn media_message(event_type: &str) -> ProgressMessage {
        let mut message = base_test_message(
            ProgressMessageKind::AgentNotification {
                event_type: event_type.to_string(),
                message: "media event".to_string(),
                entity_key: None,
                metadata: serde_json::Value::Null,
            },
            "agent:__media__",
        );
        message.event_type = Some(event_type.to_string());
        message.agent_id = Some("__media__".to_string());
        message
    }

    fn execution_message(execution_id: &str) -> ProgressMessage {
        let mut message = base_test_message(
            ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: None,
            },
            "task:task-1",
        );
        message.execution_id = Some(execution_id.to_string());
        message.task_id = Some("task-1".to_string());
        message.root_task_id = Some("task-1".to_string());
        // `SubscriptionFilter::TaskId` matches on routing keys, which the
        // normalizer derives; a hand-built message must carry them itself.
        message.routing_keys = vec!["task/task-1".to_string()];
        message
    }

    #[tokio::test]
    async fn a_media_heartbeat_is_broadcast_but_never_journaled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (router, workspace_layout) = test_router(tmp.path()).await;
        let shard = workspace_layout
            .progress_events_dir(PRINCIPAL, WORKSPACE)
            .join("agent:__media__.jsonl");
        let mut stream = router.progress_stream();

        process_message(&router.inner, media_message(MEDIA_SESSION_HEARTBEAT))
            .await
            .expect("heartbeat");

        assert!(
            !shard.exists(),
            "a heartbeat must not create or grow the media shard"
        );
        assert!(
            stream.try_recv().is_ok(),
            "a heartbeat must still reach live subscribers"
        );

        process_message(&router.inner, media_message(MEDIA_SESSION_REGISTERED))
            .await
            .expect("registration");

        let body = std::fs::read_to_string(&shard).expect("shard after a durable media event");
        assert_eq!(
            body.lines().filter(|line| !line.trim().is_empty()).count(),
            1,
            "only the non-ephemeral event may be journaled"
        );
        assert!(body.contains(MEDIA_SESSION_REGISTERED));
        assert!(!body.contains(MEDIA_SESSION_HEARTBEAT));
    }

    #[tokio::test]
    async fn a_heartbeat_never_becomes_an_unresolvable_pending_retry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (router, _) = test_router(tmp.path()).await;
        let subscription_id = router
            .subscribe(Subscription {
                id: String::new(),
                channel_id: "webhook".to_string(),
                filter: SubscriptionFilter::AgentId("__media__".to_string()),
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                min_severity: ProgressSeverity::Trace,
                metadata: HashMap::new(),
                source: SubscriptionSource::Dynamic,
                retention_secs: -1,
                message_template: None,
                output_severity: None,
                watermark: 0,
                pending_retry: Default::default(),
                created_at: 0,
            })
            .await
            .expect("subscribe");

        router
            .record_delivery_failure(&subscription_id, &media_message(MEDIA_SESSION_HEARTBEAT))
            .await
            .expect("record failure");

        let subscription = router
            .get_subscription(&subscription_id)
            .await
            .expect("subscription");
        assert!(
            subscription.pending_retry.is_empty(),
            "replay loads by locator from the shard; an unjournaled message has none"
        );
    }

    #[tokio::test]
    async fn a_registration_failure_is_still_queued_for_replay() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (router, _) = test_router(tmp.path()).await;
        let subscription_id = router
            .subscribe(Subscription {
                id: String::new(),
                channel_id: "webhook".to_string(),
                filter: SubscriptionFilter::AgentId("__media__".to_string()),
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                min_severity: ProgressSeverity::Trace,
                metadata: HashMap::new(),
                source: SubscriptionSource::Dynamic,
                retention_secs: -1,
                message_template: None,
                output_severity: None,
                watermark: 0,
                pending_retry: Default::default(),
                created_at: 0,
            })
            .await
            .expect("subscribe");

        router
            .record_delivery_failure(&subscription_id, &media_message(MEDIA_SESSION_REGISTERED))
            .await
            .expect("record failure");

        let subscription = router
            .get_subscription(&subscription_id)
            .await
            .expect("subscription");
        assert_eq!(
            subscription.pending_retry.len(),
            1,
            "a journaled message keeps its replay guarantee"
        );
    }

    #[tokio::test]
    async fn many_messages_coalesce_into_one_lineage_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (router, workspace_layout) = test_router(tmp.path()).await;
        let lineage_path = workspace_layout.progress_lineage_path(PRINCIPAL, WORKSPACE);

        for entry in 0..32_u32 {
            process_message(&router.inner, execution_message(&format!("exec-{entry}")))
                .await
                .expect("message");
        }

        assert!(
            !lineage_path.exists(),
            "32 messages must not produce 32 whole-map rewrites"
        );

        router
            .inner
            .lineage_index
            .flush_if_dirty()
            .await
            .expect("flush");

        let written: HashMap<String, ExecutionLineage> =
            serde_json::from_slice(&std::fs::read(&lineage_path).expect("read lineage index"))
                .expect("parse lineage index");
        assert_eq!(
            written.len(),
            32,
            "the one write must carry everything the messages left"
        );
        assert!(written.contains_key("exec-31"));
    }

    #[tokio::test]
    async fn a_watermark_advance_rides_the_slow_lane_only() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (router, _) = test_router(tmp.path()).await;
        router
            .subscribe(Subscription {
                id: String::new(),
                channel_id: "webhook".to_string(),
                filter: SubscriptionFilter::TaskId("task-1".to_string()),
                principal: PRINCIPAL.to_string(),
                workspace: WORKSPACE.to_string(),
                min_severity: ProgressSeverity::Trace,
                metadata: HashMap::new(),
                source: SubscriptionSource::Dynamic,
                retention_secs: -1,
                message_template: None,
                output_severity: None,
                watermark: 0,
                pending_retry: Default::default(),
                created_at: 0,
            })
            .await
            .expect("subscribe");
        router
            .inner
            .subscriptions_dirty
            .store(false, Ordering::SeqCst);
        router
            .inner
            .subscription_watermarks_dirty
            .store(false, Ordering::SeqCst);

        process_message(&router.inner, execution_message("exec-1"))
            .await
            .expect("matched message");

        assert!(
            router
                .inner
                .subscription_watermarks_dirty
                .load(Ordering::SeqCst),
            "a matched message advances the watermark"
        );
        assert!(
            !router.inner.subscriptions_dirty.load(Ordering::SeqCst),
            "a watermark advance alone must not schedule a fast-lane rewrite"
        );
    }
}
