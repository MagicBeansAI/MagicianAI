//! Foreground query embedding micro-batcher.
//!
//! Physical embedding capacity stays one. Distinct same-contract queries that
//! are already queued can share one `/api/embed` call. A lone waiter never
//! waits for the gathering window, so a single chat miss is not taxed. Two or
//! more distinct waiters wait the window once (`now + window_ms`), then flush;
//! the window does not restart until the query deadline.
//! Crate `Default` stays pass-through (`window_ms = 0`, `max_items = 1`) so
//! unit tests that never install Magician config do not batch. Magician YAML
//! default is window 3 ms / max items 8.
//! `MAGICIAN_EMBEDDING_QUERY_BATCH=off` forces the crate pass-through.

use std::{
    collections::{HashMap, VecDeque},
    env,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};

use anyhow::{anyhow, Result};
use tokio::sync::{oneshot, Notify};

use crate::hol_stats;

pub const DEFAULT_QUERY_EMBED_BATCH_WINDOW_MS: u64 = 0;
pub const DEFAULT_QUERY_EMBED_BATCH_MAX_ITEMS: usize = 1;
pub const DEFAULT_QUERY_EMBED_BATCH_MAX_CHARS: usize = 6_000;

#[doc(hidden)]
pub static QUERY_EMBED_BATCH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryEmbedBatchSettings {
    pub window_ms: u64,
    pub max_items: usize,
    pub max_chars: usize,
}

impl Default for QueryEmbedBatchSettings {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_QUERY_EMBED_BATCH_WINDOW_MS,
            max_items: DEFAULT_QUERY_EMBED_BATCH_MAX_ITEMS,
            max_chars: DEFAULT_QUERY_EMBED_BATCH_MAX_CHARS,
        }
    }
}

/// Identity for one physical embedding execution contract.
/// Distinct contracts never share a batch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryEmbedContract {
    pub base_url: String,
    pub model: String,
    pub dims: usize,
    pub context_tokens: u32,
    pub batch_tokens: u32,
    pub contract_id: String,
}

type BatchOutcome = Result<Arc<Vec<f32>>, Arc<str>>;

struct Waiter {
    query: String,
    cache_key: String,
    deadline: tokio::time::Instant,
    tx: oneshot::Sender<BatchOutcome>,
}

struct ContractQueue {
    waiters: VecDeque<Waiter>,
    leader_alive: bool,
    notify: Arc<Notify>,
    /// Set once unique waiters exceed 1. Flush when `now >=` this; do not
    /// restart the window on each timer wake.
    gather_deadline: Option<tokio::time::Instant>,
}

impl Default for ContractQueue {
    fn default() -> Self {
        Self {
            waiters: VecDeque::new(),
            leader_alive: false,
            notify: Arc::new(Notify::new()),
            gather_deadline: None,
        }
    }
}

struct BatcherState {
    settings: QueryEmbedBatchSettings,
    queues: HashMap<QueryEmbedContract, ContractQueue>,
}

static BATCH_ENABLED: AtomicBool = AtomicBool::new(false);

fn batcher_state() -> &'static Mutex<BatcherState> {
    static STATE: OnceLock<Mutex<BatcherState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(BatcherState {
            settings: QueryEmbedBatchSettings::default(),
            queues: HashMap::new(),
        })
    })
}

fn lock_state() -> std::sync::MutexGuard<'static, BatcherState> {
    batcher_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_batch_override() -> Option<bool> {
    let value = env::var("MAGICIAN_EMBEDDING_QUERY_BATCH").ok()?;
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("pass_through")
        || trimmed.eq_ignore_ascii_case("disabled")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed == "0"
    {
        Some(false)
    } else if trimmed.eq_ignore_ascii_case("on")
        || trimmed.eq_ignore_ascii_case("enabled")
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed == "1"
    {
        Some(true)
    } else {
        None
    }
}

fn settings_enable_batching(settings: &QueryEmbedBatchSettings) -> bool {
    settings.max_items > 1 || settings.window_ms > 0
}

pub fn query_embed_batch_enabled() -> bool {
    match env_batch_override() {
        Some(false) => false,
        Some(true) => true,
        None => BATCH_ENABLED.load(Ordering::Acquire),
    }
}

pub fn query_embed_batch_settings() -> QueryEmbedBatchSettings {
    lock_state().settings
}

pub fn install_query_embed_batch(settings: QueryEmbedBatchSettings) {
    let normalized = QueryEmbedBatchSettings {
        window_ms: settings.window_ms.min(50),
        max_items: settings.max_items.max(1),
        max_chars: settings.max_chars.max(1),
    };
    let enabled = settings_enable_batching(&normalized);
    BATCH_ENABLED.store(enabled, Ordering::Release);
    let mut state = lock_state();
    state.settings = normalized;
    if !enabled {
        fail_all_waiters(&mut state, "query embedding batcher disabled");
    }
}

pub fn reset_query_embed_batch_for_tests() {
    BATCH_ENABLED.store(false, Ordering::Release);
    let mut state = lock_state();
    fail_all_waiters(&mut state, "query embedding batcher reset");
    *state = BatcherState {
        settings: QueryEmbedBatchSettings::default(),
        queues: HashMap::new(),
    };
    hol_stats::set_query_embed_batch_waiters(0);
}

fn fail_all_waiters(state: &mut BatcherState, message: &str) {
    let queues = std::mem::take(&mut state.queues);
    for queue in queues.into_values() {
        for waiter in queue.waiters {
            let _ = waiter.tx.send(Err(Arc::<str>::from(message)));
        }
    }
}

fn publish_waiter_count(state: &BatcherState) {
    let waiters = state
        .queues
        .values()
        .map(|queue| queue.waiters.len())
        .sum::<usize>();
    hol_stats::set_query_embed_batch_waiters(waiters);
}

/// Join a same-contract query batch. `load_batch` runs once per physical
/// `/api/embed` call with unique texts in waiter order.
pub async fn join_query_embed_batch<F, Fut>(
    contract: QueryEmbedContract,
    query: String,
    cache_key: String,
    deadline: tokio::time::Instant,
    load_batch: F,
) -> Result<Vec<f32>>
where
    F: Fn(Vec<String>, tokio::time::Instant) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<Vec<f32>>>> + Send + 'static,
{
    let (rx, become_leader, notify) = {
        let mut state = lock_state();
        let queue = state.queues.entry(contract.clone()).or_default();
        let (tx, rx) = oneshot::channel();
        queue.waiters.push_back(Waiter {
            query,
            cache_key,
            deadline,
            tx,
        });
        let become_leader = !queue.leader_alive;
        if become_leader {
            queue.leader_alive = true;
        }
        queue.notify.notify_waiters();
        let notify = Arc::clone(&queue.notify);
        publish_waiter_count(&state);
        (rx, become_leader, notify)
    };

    if become_leader {
        let load_batch = Arc::new(load_batch);
        tokio::spawn(async move {
            run_leader(contract, load_batch, notify).await;
        });
    }

    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => {
            hol_stats::record_query_embed_batch_deadline_loss();
            Err(anyhow!("query embedding exhausted the request deadline"))
        }
        result = rx => match result {
            Ok(Ok(vector)) => Ok((*vector).clone()),
            Ok(Err(error)) => Err(anyhow!(error.to_string())),
            Err(_) => Err(anyhow!("query embedding batch leader dropped")),
        }
    }
}

async fn run_leader<F, Fut>(contract: QueryEmbedContract, load_batch: Arc<F>, notify: Arc<Notify>)
where
    F: Fn(Vec<String>, tokio::time::Instant) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<Vec<f32>>>> + Send + 'static,
{
    struct LeaderGuard {
        contract: QueryEmbedContract,
        finished: bool,
    }
    impl Drop for LeaderGuard {
        fn drop(&mut self) {
            if self.finished {
                return;
            }
            let mut state = lock_state();
            if let Some(queue) = state.queues.get_mut(&self.contract) {
                queue.leader_alive = false;
                let waiters = std::mem::take(&mut queue.waiters);
                state.queues.remove(&self.contract);
                for waiter in waiters {
                    let _ = waiter.tx.send(Err(Arc::<str>::from(
                        "query embedding batch leader dropped",
                    )));
                }
            }
            publish_waiter_count(&state);
        }
    }

    let mut guard = LeaderGuard {
        contract: contract.clone(),
        finished: false,
    };
    loop {
        let Some(batch) = take_batch_or_wait(&contract, &notify).await else {
            guard.finished = true;
            return;
        };
        dispatch_batch(&contract, &load_batch, batch).await;
    }
}

async fn take_batch_or_wait(contract: &QueryEmbedContract, notify: &Notify) -> Option<Vec<Waiter>> {
    loop {
        let notified = notify.notified();
        tokio::pin!(notified);
        let _ = notified.as_mut().enable();
        let decision = {
            let mut state = lock_state();
            let settings = state.settings;
            let Some(queue) = state.queues.get_mut(contract) else {
                return None;
            };
            prune_queue(queue);
            if queue.waiters.is_empty() {
                queue.leader_alive = false;
                state.queues.remove(contract);
                publish_waiter_count(&state);
                return None;
            }
            if should_flush_now(queue, &settings) {
                queue.gather_deadline = None;
                let batch = take_batch(queue, &settings);
                publish_waiter_count(&state);
                Decision::Flush(batch)
            } else {
                let slack = earliest_remaining(&queue.waiters);
                let gather = remaining_gather(queue);
                Decision::Wait(gather.min(slack).max(Duration::from_millis(1)))
            }
        };
        match decision {
            Decision::Flush(batch) => return Some(batch),
            Decision::Wait(delay) => {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = notified => {}
                }
            },
        }
    }
}

enum Decision {
    Flush(Vec<Waiter>),
    Wait(Duration),
}

fn prune_queue(queue: &mut ContractQueue) {
    let now = tokio::time::Instant::now();
    let mut kept = VecDeque::new();
    for waiter in queue.waiters.drain(..) {
        if waiter.tx.is_closed() {
            continue;
        }
        if waiter.deadline <= now {
            hol_stats::record_query_embed_batch_deadline_loss();
            let _ = waiter.tx.send(Err(Arc::<str>::from(
                "query embedding exhausted the request deadline",
            )));
            continue;
        }
        kept.push_back(waiter);
    }
    queue.waiters = kept;
}

fn unique_preview(waiters: &VecDeque<Waiter>) -> (usize, usize) {
    let mut seen = HashMap::<&str, usize>::new();
    let mut chars = 0usize;
    for waiter in waiters {
        if seen.contains_key(waiter.cache_key.as_str()) {
            continue;
        }
        seen.insert(waiter.cache_key.as_str(), waiter.query.len());
        chars = chars.saturating_add(waiter.query.len());
    }
    (seen.len(), chars)
}

fn should_flush_now(queue: &mut ContractQueue, settings: &QueryEmbedBatchSettings) -> bool {
    if settings.max_items <= 1 || settings.window_ms == 0 {
        queue.gather_deadline = None;
        return true;
    }
    let (unique, chars) = unique_preview(&queue.waiters);
    if unique <= 1 {
        queue.gather_deadline = None;
        return true;
    }
    if unique >= settings.max_items || chars >= settings.max_chars {
        queue.gather_deadline = None;
        return true;
    }
    let now = tokio::time::Instant::now();
    let deadline = *queue
        .gather_deadline
        .get_or_insert_with(|| now + Duration::from_millis(settings.window_ms.max(1)));
    now >= deadline
}

fn remaining_gather(queue: &ContractQueue) -> Duration {
    let now = tokio::time::Instant::now();
    queue
        .gather_deadline
        .map(|deadline| deadline.saturating_duration_since(now))
        .unwrap_or(Duration::from_millis(1))
}

fn earliest_remaining(waiters: &VecDeque<Waiter>) -> Duration {
    let now = tokio::time::Instant::now();
    waiters
        .iter()
        .map(|waiter| waiter.deadline.saturating_duration_since(now))
        .min()
        .unwrap_or(Duration::ZERO)
}

fn take_batch(queue: &mut ContractQueue, settings: &QueryEmbedBatchSettings) -> Vec<Waiter> {
    let max_items = settings.max_items.max(1);
    let max_chars = settings.max_chars.max(1);
    let mut taken = Vec::new();
    let mut leftover = VecDeque::new();
    let mut unique_keys: Vec<String> = Vec::new();
    let mut unique_chars = 0usize;
    while let Some(waiter) = queue.waiters.pop_front() {
        if waiter.tx.is_closed() {
            continue;
        }
        let already = unique_keys.iter().any(|key| key == &waiter.cache_key);
        if already {
            taken.push(waiter);
            continue;
        }
        let next_chars = unique_chars.saturating_add(waiter.query.len());
        let would_exceed =
            !unique_keys.is_empty() && (unique_keys.len() >= max_items || next_chars > max_chars);
        if would_exceed {
            leftover.push_back(waiter);
            continue;
        }
        unique_chars = next_chars;
        unique_keys.push(waiter.cache_key.clone());
        taken.push(waiter);
    }
    leftover.append(&mut queue.waiters);
    queue.waiters = leftover;
    taken
}

async fn dispatch_batch<F, Fut>(
    contract: &QueryEmbedContract,
    load_batch: &Arc<F>,
    waiters: Vec<Waiter>,
) where
    F: Fn(Vec<String>, tokio::time::Instant) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<Vec<f32>>>> + Send + 'static,
{
    if waiters.is_empty() {
        return;
    }
    let mut unique_texts = Vec::new();
    let mut unique_keys = Vec::new();
    let mut waiter_slots: Vec<(Waiter, usize)> = Vec::new();
    for waiter in waiters {
        if waiter.tx.is_closed() {
            continue;
        }
        if let Some(index) = unique_keys.iter().position(|key| key == &waiter.cache_key) {
            waiter_slots.push((waiter, index));
        } else {
            let index = unique_texts.len();
            unique_texts.push(waiter.query.clone());
            unique_keys.push(waiter.cache_key.clone());
            waiter_slots.push((waiter, index));
        }
    }
    if waiter_slots.is_empty() {
        return;
    }
    // Tightest waiter owns the physical call. A background query with a
    // multi-second deadline must not keep Ollama occupied after a chat miss
    // has already given up — that is the exact HOL the batcher exists to
    // avoid. Closed waiters are skipped at take_batch; remaining ones still
    // share one sequence, but it aborts when the first deadline fires.
    let http_deadline = waiter_slots
        .iter()
        .map(|(waiter, _)| waiter.deadline)
        .min()
        .unwrap_or_else(tokio::time::Instant::now);
    hol_stats::record_query_embed_batch_dispatch(waiter_slots.len(), unique_texts.len());
    let loaded = load_batch(unique_texts.clone(), http_deadline).await;
    match loaded {
        Ok(vectors) => {
            if vectors.len() != unique_texts.len() {
                hol_stats::record_query_embed_batch_validation_failure();
                fail_waiters(
                    waiter_slots,
                    format!(
                        "query embedding batch returned {} vectors for {} inputs",
                        vectors.len(),
                        unique_texts.len()
                    ),
                );
                return;
            }
            for vector in &vectors {
                if contract.dims != 0 && vector.len() != contract.dims {
                    hol_stats::record_query_embed_batch_validation_failure();
                    fail_waiters(
                        waiter_slots,
                        format!(
                            "query embedding batch returned {}-dim vector; expected {}",
                            vector.len(),
                            contract.dims
                        ),
                    );
                    return;
                }
            }
            for (waiter, index) in waiter_slots {
                if waiter.tx.is_closed() {
                    continue;
                }
                let _ = waiter.tx.send(Ok(Arc::new(vectors[index].clone())));
            }
        },
        Err(error) => {
            fail_waiters(waiter_slots, format!("{error:#}"));
        },
    }
}

fn fail_waiters(waiters: Vec<(Waiter, usize)>, message: String) {
    let shared: Arc<str> = message.into();
    for (waiter, _) in waiters {
        let _ = waiter.tx.send(Err(Arc::clone(&shared)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::time::{sleep, timeout, Duration as TokioDuration};

    fn contract(name: &str) -> QueryEmbedContract {
        QueryEmbedContract {
            base_url: "http://127.0.0.1:9".to_string(),
            model: name.to_string(),
            dims: 2,
            context_tokens: 8,
            batch_tokens: 8,
            contract_id: name.to_string(),
        }
    }

    fn future_deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + TokioDuration::from_secs(2)
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn lone_waiter_does_not_wait_the_window() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 50,
            max_items: 8,
            max_chars: 6_000,
        });
        let started = std::time::Instant::now();
        let vector = join_query_embed_batch(
            contract("lone"),
            "hello".to_string(),
            "k-hello".to_string(),
            future_deadline(),
            |texts, _| async move {
                assert_eq!(texts, vec!["hello".to_string()]);
                Ok(vec![vec![1.0, 0.0]])
            },
        )
        .await
        .unwrap();
        assert_eq!(vector, vec![1.0, 0.0]);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(40),
            "a single waiter must not pay the gathering window"
        );
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn physical_embed_deadline_is_the_tightest_waiter() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 3,
            max_items: 8,
            max_chars: 6_000,
        });
        let observed = Arc::new(Mutex::new(None::<tokio::time::Instant>));
        let release = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());
        let first_release = Arc::clone(&release);
        let first_started_flag = Arc::clone(&first_started);
        let first_observed = Arc::clone(&observed);
        let first = tokio::spawn(async move {
            join_query_embed_batch(
                contract("tight-deadline"),
                "warmup".to_string(),
                "k-warmup".to_string(),
                future_deadline(),
                move |texts, deadline| {
                    let release = Arc::clone(&first_release);
                    let started = Arc::clone(&first_started_flag);
                    let observed = Arc::clone(&first_observed);
                    async move {
                        if texts.len() == 1 && texts[0] == "warmup" {
                            started.notify_waiters();
                            release.notified().await;
                        } else if texts.len() >= 2 {
                            *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(deadline);
                        }
                        Ok(vec![vec![1.0, 0.0]; texts.len()])
                    }
                },
            )
            .await
        });
        timeout(TokioDuration::from_secs(1), first_started.notified())
            .await
            .expect("warmup physical call");
        let started = tokio::time::Instant::now();
        let tight = started + TokioDuration::from_secs(2);
        let loose = started + TokioDuration::from_secs(8);

        let tight_observed = Arc::clone(&observed);
        let tight_join = tokio::spawn(async move {
            join_query_embed_batch(
                contract("tight-deadline"),
                "alpha".to_string(),
                "k-alpha".to_string(),
                tight,
                move |texts, deadline| {
                    let observed = Arc::clone(&tight_observed);
                    async move {
                        if texts.len() >= 2 {
                            *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(deadline);
                        }
                        Ok(vec![vec![1.0, 0.0]; texts.len()])
                    }
                },
            )
            .await
        });
        let loose_observed = Arc::clone(&observed);
        let loose_join = tokio::spawn(async move {
            join_query_embed_batch(
                contract("tight-deadline"),
                "beta".to_string(),
                "k-beta".to_string(),
                loose,
                move |texts, deadline| {
                    let observed = Arc::clone(&loose_observed);
                    async move {
                        if texts.len() >= 2 {
                            *observed.lock().unwrap_or_else(|p| p.into_inner()) = Some(deadline);
                        }
                        Ok(vec![vec![0.0, 1.0]; texts.len()])
                    }
                },
            )
            .await
        });
        sleep(TokioDuration::from_millis(8)).await;
        release.notify_waiters();
        timeout(TokioDuration::from_millis(400), first)
            .await
            .expect("warmup")
            .expect("warmup task")
            .expect("warmup embed");
        timeout(TokioDuration::from_millis(400), tight_join)
            .await
            .expect("tight waiter")
            .expect("tight task")
            .expect("tight embed");
        timeout(TokioDuration::from_millis(400), loose_join)
            .await
            .expect("loose waiter")
            .expect("loose task")
            .expect("loose embed");
        let deadline = observed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expect("mixed-batch physical deadline");
        let slack = deadline.saturating_duration_since(tight);
        assert!(
            slack <= TokioDuration::from_millis(5),
            "physical embed must abort with the chat miss, not the 5s background waiter; slack={slack:?}"
        );
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn magician_default_window_flushes_queued_distinct_queries_without_the_query_deadline() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 3,
            max_items: 8,
            max_chars: 6_000,
        });
        let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let release = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());

        let first_calls = Arc::clone(&calls);
        let first_release = Arc::clone(&release);
        let first_started_flag = Arc::clone(&first_started);
        let first = tokio::spawn(async move {
            join_query_embed_batch(
                contract("window"),
                "alpha".to_string(),
                "k-alpha".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&first_calls);
                    let release = Arc::clone(&first_release);
                    let started = Arc::clone(&first_started_flag);
                    async move {
                        let call_index = {
                            let mut recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
                            recorded.push(texts.clone());
                            recorded.len()
                        };
                        if call_index == 1 {
                            started.notify_waiters();
                            release.notified().await;
                            Ok(vec![vec![1.0, 0.0]; texts.len()])
                        } else {
                            Ok(texts
                                .iter()
                                .enumerate()
                                .map(|(index, _)| vec![index as f32, 1.0])
                                .collect::<Vec<_>>())
                        }
                    }
                },
            )
            .await
        });
        timeout(TokioDuration::from_secs(1), first_started.notified())
            .await
            .expect("first physical call should start");

        let second_calls = Arc::clone(&calls);
        let second = tokio::spawn(async move {
            join_query_embed_batch(
                contract("window"),
                "beta".to_string(),
                "k-beta".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&second_calls);
                    async move {
                        calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(texts.clone());
                        Ok(texts
                            .iter()
                            .enumerate()
                            .map(|(index, _)| vec![index as f32, 1.0])
                            .collect::<Vec<_>>())
                    }
                },
            )
            .await
        });
        let third_calls = Arc::clone(&calls);
        let third = tokio::spawn(async move {
            join_query_embed_batch(
                contract("window"),
                "gamma".to_string(),
                "k-gamma".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&third_calls);
                    async move {
                        calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(texts.clone());
                        Ok(texts
                            .iter()
                            .enumerate()
                            .map(|(index, _)| vec![index as f32, 1.0])
                            .collect::<Vec<_>>())
                    }
                },
            )
            .await
        });
        sleep(TokioDuration::from_millis(5)).await;
        let flushed_at = std::time::Instant::now();
        release.notify_waiters();

        let first = timeout(TokioDuration::from_millis(200), first)
            .await
            .expect("first join must not wait the query deadline")
            .expect("first task");
        let second = timeout(TokioDuration::from_millis(200), second)
            .await
            .expect("second join must flush after the 3ms window, not the 5s deadline")
            .expect("second task");
        let third = timeout(TokioDuration::from_millis(200), third)
            .await
            .expect("third join must flush after the 3ms window, not the 5s deadline")
            .expect("third task");
        assert_eq!(first.unwrap(), vec![1.0, 0.0]);
        assert!(second.is_ok());
        assert!(third.is_ok());
        assert!(
            flushed_at.elapsed() < std::time::Duration::from_millis(50),
            "Magician default window is 3ms; overlapping waiters must not stall until the query deadline"
        );
        let recorded = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded.len(),
            2,
            "one in-flight call then one windowed batch"
        );
        assert_eq!(recorded[0], vec!["alpha".to_string()]);
        assert_eq!(recorded[1].len(), 2);
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn queued_distinct_queries_share_one_physical_call() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let release = Arc::new(tokio::sync::Notify::new());
        let first_started = Arc::new(tokio::sync::Notify::new());

        let first_calls = Arc::clone(&calls);
        let first_release = Arc::clone(&release);
        let first_started_flag = Arc::clone(&first_started);
        let first = tokio::spawn(async move {
            join_query_embed_batch(
                contract("share"),
                "alpha".to_string(),
                "k-alpha".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&first_calls);
                    let release = Arc::clone(&first_release);
                    let started = Arc::clone(&first_started_flag);
                    async move {
                        let call_index = {
                            let mut recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
                            recorded.push(texts.clone());
                            recorded.len()
                        };
                        if call_index == 1 {
                            started.notify_waiters();
                            release.notified().await;
                            Ok(vec![vec![1.0, 0.0]; texts.len()])
                        } else {
                            Ok(texts
                                .iter()
                                .enumerate()
                                .map(|(index, _)| vec![index as f32, 1.0])
                                .collect::<Vec<_>>())
                        }
                    }
                },
            )
            .await
        });
        timeout(TokioDuration::from_secs(1), first_started.notified())
            .await
            .expect("first physical call should start");

        let second_calls = Arc::clone(&calls);
        let second = tokio::spawn(async move {
            join_query_embed_batch(
                contract("share"),
                "beta".to_string(),
                "k-beta".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&second_calls);
                    async move {
                        calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(texts.clone());
                        Ok(texts
                            .iter()
                            .enumerate()
                            .map(|(index, _)| vec![index as f32, 1.0])
                            .collect::<Vec<_>>())
                    }
                },
            )
            .await
        });
        let third_calls = Arc::clone(&calls);
        let third = tokio::spawn(async move {
            join_query_embed_batch(
                contract("share"),
                "gamma".to_string(),
                "k-gamma".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&third_calls);
                    async move {
                        calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(texts.clone());
                        Ok(texts
                            .iter()
                            .enumerate()
                            .map(|(index, _)| vec![index as f32, 1.0])
                            .collect::<Vec<_>>())
                    }
                },
            )
            .await
        });
        sleep(TokioDuration::from_millis(20)).await;
        release.notify_waiters();

        let first = timeout(TokioDuration::from_secs(1), first)
            .await
            .expect("first join")
            .expect("first task");
        let second = timeout(TokioDuration::from_secs(1), second)
            .await
            .expect("second join")
            .expect("second task");
        let third = timeout(TokioDuration::from_secs(1), third)
            .await
            .expect("third join")
            .expect("third task");
        assert_eq!(first.unwrap(), vec![1.0, 0.0]);
        let second = second.unwrap();
        let third = third.unwrap();
        assert_ne!(second, third, "distinct queries keep positional vectors");
        let recorded = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            recorded.len(),
            2,
            "one in-flight call then one batched call"
        );
        assert_eq!(recorded[0], vec!["alpha".to_string()]);
        assert_eq!(recorded[1].len(), 2);
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn identical_keys_collapse_to_one_input() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());

        let first_calls = Arc::clone(&calls);
        let first_release = Arc::clone(&release);
        let first_started = Arc::clone(&started);
        let first = tokio::spawn(async move {
            join_query_embed_batch(
                contract("same"),
                "shared".to_string(),
                "k-shared".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&first_calls);
                    let release = Arc::clone(&first_release);
                    let started = Arc::clone(&first_started);
                    async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            started.notify_waiters();
                            release.notified().await;
                        }
                        Ok(vec![vec![4.0, 1.0]; texts.len()])
                    }
                },
            )
            .await
        });
        timeout(TokioDuration::from_secs(1), started.notified())
            .await
            .unwrap();

        let second = tokio::spawn(async move {
            join_query_embed_batch(
                contract("same"),
                "shared".to_string(),
                "k-shared".to_string(),
                future_deadline(),
                |_texts, _| async move { Ok(vec![vec![9.0, 9.0]]) },
            )
            .await
        });
        let third = tokio::spawn(async move {
            join_query_embed_batch(
                contract("same"),
                "shared".to_string(),
                "k-shared".to_string(),
                future_deadline(),
                |_texts, _| async move { Ok(vec![vec![8.0, 8.0]]) },
            )
            .await
        });
        sleep(TokioDuration::from_millis(20)).await;
        release.notify_waiters();
        let first = first.await.unwrap().unwrap();
        let second = second.await.unwrap().unwrap();
        let third = third.await.unwrap().unwrap();
        assert_eq!(first, vec![4.0, 1.0]);
        assert_eq!(second, vec![4.0, 1.0]);
        assert_eq!(third, vec![4.0, 1.0]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn batch_failure_fans_out_and_allows_retry() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_a = Arc::clone(&attempts);
        let err = join_query_embed_batch(
            contract("fail"),
            "q".to_string(),
            "k-q".to_string(),
            future_deadline(),
            move |_texts, _| {
                let attempts = Arc::clone(&attempts_a);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow!("provider down"))
                }
            },
        )
        .await
        .expect_err("first call fails");
        assert!(err.to_string().contains("provider down"));
        let attempts_b = Arc::clone(&attempts);
        let retry = join_query_embed_batch(
            contract("fail"),
            "q".to_string(),
            "k-q".to_string(),
            future_deadline(),
            move |_texts, _| {
                let attempts = Arc::clone(&attempts_b);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(vec![vec![0.0, 1.0]])
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(retry, vec![0.0, 1.0]);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn cancelled_waiter_is_dropped_before_dispatch() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let calls = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(tokio::sync::Notify::new());
        let first_calls = Arc::clone(&calls);
        let first_release = Arc::clone(&release);
        let first_started = Arc::clone(&started);
        let first = tokio::spawn(async move {
            join_query_embed_batch(
                contract("cancel"),
                "keep".to_string(),
                "k-keep".to_string(),
                future_deadline(),
                move |texts, _| {
                    let calls = Arc::clone(&first_calls);
                    let release = Arc::clone(&first_release);
                    let started = Arc::clone(&first_started);
                    async move {
                        let call_index = {
                            let mut recorded = calls.lock().unwrap_or_else(|p| p.into_inner());
                            recorded.push(texts.clone());
                            recorded.len()
                        };
                        if call_index == 1 {
                            started.notify_waiters();
                            release.notified().await;
                        }
                        Ok(vec![vec![1.0, 0.0]; texts.len()])
                    }
                },
            )
            .await
        });
        timeout(TokioDuration::from_secs(1), started.notified())
            .await
            .unwrap();
        let cancelled = tokio::spawn(async move {
            join_query_embed_batch(
                contract("cancel"),
                "drop-me".to_string(),
                "k-drop".to_string(),
                future_deadline(),
                |_texts, _| async move { Ok(vec![vec![9.0, 9.0]]) },
            )
            .await
        });
        sleep(TokioDuration::from_millis(10)).await;
        cancelled.abort();
        let _ = cancelled.await;
        let survivor = tokio::spawn(async move {
            join_query_embed_batch(
                contract("cancel"),
                "stay".to_string(),
                "k-stay".to_string(),
                future_deadline(),
                |texts, _| async move { Ok(vec![vec![2.0, 0.0]; texts.len()]) },
            )
            .await
        });
        sleep(TokioDuration::from_millis(15)).await;
        release.notify_waiters();
        assert!(first.await.unwrap().is_ok());
        let stay = survivor.await.unwrap().unwrap();
        assert_eq!(stay, vec![1.0, 0.0]);
        let recorded = calls.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(recorded[0], vec!["keep".to_string()]);
        assert!(
            recorded
                .get(1)
                .is_none_or(|batch| !batch.iter().any(|text| text == "drop-me")),
            "cancelled waiter must not appear in the next physical call"
        );
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn wrong_vector_count_is_not_distributed() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let error = join_query_embed_batch(
            contract("validate"),
            "q".to_string(),
            "k".to_string(),
            future_deadline(),
            |_texts, _| async move { Ok(vec![vec![1.0, 0.0], vec![0.0, 1.0]]) },
        )
        .await
        .expect_err("count mismatch");
        assert!(error.to_string().contains("returned 2 vectors"));
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn env_off_disables_batching_flag() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 5,
            max_items: 8,
            max_chars: 6_000,
        });
        assert!(query_embed_batch_enabled());
        let _guard = EnvVarGuard::set("MAGICIAN_EMBEDDING_QUERY_BATCH", "off");
        assert!(!query_embed_batch_enabled());
        reset_query_embed_batch_for_tests();
    }

    #[tokio::test]
    async fn different_contracts_do_not_share_a_batch() {
        let _lock = QUERY_EMBED_BATCH_TEST_LOCK.lock().await;
        reset_query_embed_batch_for_tests();
        install_query_embed_batch(QueryEmbedBatchSettings {
            window_ms: 0,
            max_items: 8,
            max_chars: 6_000,
        });
        let a = join_query_embed_batch(
            contract("model-a"),
            "q".to_string(),
            "k".to_string(),
            future_deadline(),
            |texts, _| async move {
                assert_eq!(texts.len(), 1);
                Ok(vec![vec![1.0, 0.0]])
            },
        );
        let b = join_query_embed_batch(
            contract("model-b"),
            "q".to_string(),
            "k".to_string(),
            future_deadline(),
            |texts, _| async move {
                assert_eq!(texts.len(), 1);
                Ok(vec![vec![0.0, 1.0]])
            },
        );
        let (a, b) = tokio::join!(a, b);
        assert_eq!(a.unwrap(), vec![1.0, 0.0]);
        assert_eq!(b.unwrap(), vec![0.0, 1.0]);
        reset_query_embed_batch_for_tests();
    }
}
