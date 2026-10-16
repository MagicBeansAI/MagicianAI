use crate::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace, realtime_events::RuntimeTransportEvent,
};
use magician_core::decision_metrics::{store, types::*};
use magicllm::LlmScope;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, Semaphore};

const QUEUE: usize = 128;
const MAX_EVENT: usize = 32 * 1024;
static CAPTURE: OnceLock<Mutex<Weak<Inner>>> = OnceLock::new();
#[derive(Clone)]
pub struct DecisionMetrics(Arc<Inner>);
struct Inner {
    layout: ArtifactV2Workspace,
    sender: SyncSender<Message>,
    dropped: AtomicU64,
    errors: AtomicU64,
    stopping: AtomicBool,
    readers: Arc<Semaphore>,
    cache: Mutex<VecDeque<(String, Instant, Dashboard)>>,
}
enum Message {
    Event(LlmScope, i64, Value),
    Review(LlmScope, ReviewUpdate, oneshot::Sender<Result<u64, String>>),
    Stop(oneshot::Sender<()>),
}
impl DecisionMetrics {
    /// No directories, databases or history are opened on the startup thread.
    pub fn start(layout: ArtifactV2Workspace) -> Self {
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let inner = Arc::new(Inner {
            layout,
            sender: tx,
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            readers: Arc::new(Semaphore::new(4)),
            cache: Mutex::new(VecDeque::new()),
        });
        *CAPTURE
            .get_or_init(|| Mutex::new(Weak::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&inner);
        let weak = Arc::downgrade(&inner);
        std::thread::Builder::new()
            .name("decision-metrics".into())
            .spawn(move || {
                // One 512KiB SQLite cache, shared serially across scopes. Idle scopes
                // have no open connection and queries never wait behind journal replay.
                let mut opened: Option<(LlmScope, rusqlite::Connection)> = None;
                let mut events = 0_u64;
                loop {
                    let message = match rx.recv_timeout(Duration::from_secs(60)) {
                        Ok(message) => message,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if let Some((_, conn)) = &opened {
                                if store::retain(conn, chrono::Utc::now().timestamp_millis())
                                    .is_err()
                                {
                                    if let Some(inner) = weak.upgrade() {
                                        inner.errors.fetch_add(1, Ordering::Relaxed);
                                    }
                                }
                            }
                            continue;
                        },
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if let Message::Stop(done) = message {
                        let _ = done.send(());
                        break;
                    }
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    let scope = match &message {
                        Message::Event(s, ..) | Message::Review(s, ..) => s.clone(),
                        _ => unreachable!(),
                    };
                    let result = (|| -> Result<(), String> {
                        if !scope.is_valid()
                            || !inner
                                .layout
                                .scope_root(&scope.principal, &scope.workspace)
                                .is_dir()
                        {
                            return Err("Scope unavailable".into());
                        }
                        if opened.as_ref().is_none_or(|(s, _)| s != &scope) {
                            let path = inner.layout.analytics_decision_metrics_path(
                                &scope.principal,
                                &scope.workspace,
                            );
                            std::fs::create_dir_all(path.parent().unwrap())
                                .map_err(|e| e.to_string())?;
                            opened = Some((scope.clone(), store::open(&path, true)?));
                        }
                        let conn = &mut opened.as_mut().unwrap().1;
                        match message {
                            Message::Event(_, at, data) => store::event(conn, at, &data)?,
                            Message::Review(_, update, done) => {
                                let result = store::review(
                                    conn,
                                    &update,
                                    chrono::Utc::now().timestamp_millis(),
                                );
                                let _ = done.send(result);
                            },
                            _ => unreachable!(),
                        }
                        events += 1;
                        if events % 64 == 0 {
                            store::retain(conn, chrono::Utc::now().timestamp_millis())?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        inner.errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(%error,"Decision metrics write failed");
                    }
                }
            })
            .expect("decision metrics writer thread");
        Self(inner)
    }
    pub async fn summary(&self, scope: LlmScope, q: MetricsQuery) -> Result<Dashboard, String> {
        q.validate()?;
        if !scope.is_valid() {
            return Err("Invalid scope".into());
        }
        let key = format!(
            "{}:{}:{:?}:{:?}:{:?}:{:?}",
            scope.principal, scope.workspace, q.days, q.operation, q.model, q.cohort
        );
        {
            let cache = self.0.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, _, value)) = cache
                .iter()
                .find(|(k, at, _)| k == &key && at.elapsed() < Duration::from_secs(5))
            {
                let mut value = value.clone();
                value.capture_dropped = self.0.dropped.load(Ordering::Relaxed);
                value.write_errors = self.0.errors.load(Ordering::Relaxed);
                return Ok(value);
            }
        }
        let permit = Arc::clone(&self.0.readers)
            .try_acquire_owned()
            .map_err(|_| "Dashboard busy; retry shortly".to_string())?;
        let inner = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let start = Instant::now();
            let now = chrono::Utc::now().timestamp_millis();
            let path = inner
                .layout
                .analytics_decision_metrics_path(&scope.principal, &scope.workspace);
            let (rows, truncated, collected_since_ms, last_comparison_ms) = if path.is_file() {
                let conn = store::open(&path, false)?;
                let (rows, truncated, started) = store::summary(&conn, &q, now)?;
                (
                    rows,
                    truncated,
                    started,
                    store::last_comparison(&conn, q.operation.as_deref())?,
                )
            } else {
                (vec![], false, None, None)
            };
            let value = Dashboard {
                rows,
                truncated,
                collected_since_ms,
                generated_at_ms: now,
                last_comparison_ms,
                capture_dropped: inner.dropped.load(Ordering::Relaxed),
                write_errors: inner.errors.load(Ordering::Relaxed),
                read_ms: start.elapsed().as_secs_f64() * 1000.0,
            };
            let mut cache = inner.cache.lock().unwrap_or_else(|e| e.into_inner());
            if cache.len() >= 8 {
                cache.pop_front();
            }
            if value.rows.len() <= 128 {
                cache.push_back((key, Instant::now(), value.clone()));
            }
            Ok(value)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub async fn cases(&self, scope: LlmScope, q: MetricsQuery) -> Result<Value, String> {
        q.validate()?;
        if !scope.is_valid() {
            return Err("Invalid scope".into());
        }
        let permit = Arc::clone(&self.0.readers)
            .try_acquire_owned()
            .map_err(|_| "Dashboard busy; retry shortly".to_string())?;
        let inner = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let path = inner
                .layout
                .analytics_decision_metrics_path(&scope.principal, &scope.workspace);
            if !path.is_file() {
                return Ok(json!({"cases":[],"next_before":null}));
            }
            let cases = store::cases(&store::open(&path, false)?, &q, chrono::Utc::now().timestamp_millis())?;
            Ok(json!({"next_before":cases.last().map(|c|c.at_ms),"next_before_id":cases.last().map(|c|c.id.clone()),"cases":cases}))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    pub async fn review(&self, scope: LlmScope, update: ReviewUpdate) -> Result<u64, String> {
        if self.0.stopping.load(Ordering::Acquire) {
            return Err("Metrics writer unavailable".into());
        }
        if !scope.is_valid()
            || update.comparison_id.len() > 256
            || update.labels.len() > 16
            || serde_json::to_vec(&update.labels)
                .map_err(|e| e.to_string())?
                .len()
                > 8192
        {
            return Err("Invalid review".into());
        }
        let (tx, rx) = oneshot::channel();
        self.0
            .sender
            .try_send(Message::Review(scope, update, tx))
            .map_err(|_| "Metrics writer busy; retry".to_string())?;
        let result = tokio::time::timeout(Duration::from_secs(3), rx)
            .await
            .map_err(|_| "Review confirmation timed out; reload before retrying".to_string())?
            .map_err(|_| "Metrics writer unavailable".to_string())?;
        if result.is_ok() {
            self.0
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        result
    }
    pub async fn shutdown(&self) {
        if self.0.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        let (tx, rx) = oneshot::channel();
        let mut stop = Message::Stop(tx);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            match self.0.sender.try_send(stop) {
                Ok(()) => {
                    let _ = tokio::time::timeout_at(deadline, rx).await;
                    break;
                },
                Err(mpsc::TrySendError::Full(message)) => {
                    if tokio::time::Instant::now() >= deadline {
                        self.0.errors.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    stop = message;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                },
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
    }
}
/// The producer only compacts/enqueues; disk I/O is never on the caller's path.
pub fn capture(event: &RuntimeTransportEvent) {
    let Some(inner) = CAPTURE.get().and_then(|s| s.lock().ok()?.upgrade()) else {
        return;
    };
    if inner.stopping.load(Ordering::Acquire) {
        inner.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let (scope, at, mut data) = match event {
        RuntimeTransportEvent::DecisionShadowAgreement {
            principal,
            workspace,
            record,
            timestamp,
        } => {
            let mut v = (**record).clone();
            // Full probability vectors are unnecessary for the dashboard. Retain
            // option IDs (review vocabulary), typed values and confidence only.
            if v.get("stage").is_none()
                && v["items"].as_array().is_some_and(|items| items.is_empty())
            {
                v["items"] = json!(v["offered_item_ids"].as_array().unwrap_or(&Vec::new()).iter().map(|id|json!({"item_id":id,"status":"not_started","response":null,"eligible_answers":{}})).collect::<Vec<_>>());
            }
            if let Some(items) = v["items"].as_array_mut() {
                for item in items {
                    if let Some(answers) = item["response"]["answers"].as_object_mut() {
                        for answer in answers.values_mut() {
                            if let Some(obj) = answer.as_object_mut() {
                                if let Some(p) = obj.remove("probabilities") {
                                    obj.insert(
                                        "options".into(),
                                        json!(p
                                            .as_object()
                                            .map(|m| m.keys().collect::<Vec<_>>())
                                            .unwrap_or_default()),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            if let Some(obj) = v.as_object_mut() {
                obj.remove("calls");
                obj.remove("pricing");
                obj.remove("scope");
            }
            (LlmScope::new(principal, workspace), *timestamp, v)
        },
        RuntimeTransportEvent::LLMResponseReceived {
            principal: Some(p),
            workspace: Some(w),
            correlation: Some(c),
            operation,
            provider,
            model,
            cost,
            latency_ms,
            success,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            timestamp,
            started_at_ms,
            response_kind,
            ..
        } if response_kind == "decision_model" => {
            let a = c.usage_availability;
            (
                LlmScope::new(p, w),
                *timestamp,
                json!({"stage":"usage","operation":operation,"call":{"id":c.llm_call_id,"provider":provider,"model":model,"pricing_version":magicllm::pricing::active_table().pricing_version_at(&magicllm::LLMProviderKind::from_str(provider),model,*started_at_ms),"cost_usd":a.and_then(|a|a.cost.then_some(*cost)),"latency_ms":latency_ms,"success":success,"input_tokens":a.and_then(|a|a.tokens.then_some(*input_tokens)),"output_tokens":a.and_then(|a|a.tokens.then_some(*output_tokens)),"cache_read_tokens":a.and_then(|a|a.cache_read.then_some(*cache_read_tokens)),"cache_write_tokens":a.and_then(|a|a.cache_write.then_some(*cache_creation_tokens))}}),
            )
        },
        _ => return,
    };
    let now = chrono::Utc::now().timestamp_millis();
    if !scope.is_valid() || at < now - 7 * store::DAY || at > now + 300000 {
        inner.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Never retain producer error bodies, execution text or arbitrary state.
    if let Some(obj) = data.as_object_mut() {
        obj.remove("error");
    }
    if serde_json::to_vec(&data).map_or(true, |v| v.len() > MAX_EVENT) {
        inner.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if inner
        .sender
        .try_send(Message::Event(scope, at, data))
        .is_err()
    {
        inner.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
