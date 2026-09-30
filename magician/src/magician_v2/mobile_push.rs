//! Device-bound mobile push registrations and bounded event delivery.
//!
//! Push tokens are routing identifiers, not mobile API credentials. They live
//! in a private store separate from the public paired-device roster and are
//! never accepted with caller-supplied scope or device identity.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use futures_util::{stream, StreamExt as _};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tracing::{info, warn};

use crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode;
use crate::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};

const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_BINDING_BYTES: usize = 512;
const MAX_REGISTRATIONS_PER_DEVICE: usize = 16;
const MAX_REGISTRATIONS_TOTAL: usize = 2_048;
const EVENT_CONCURRENCY: usize = 8;
const DELIVERY_CONCURRENCY: usize = 8;
const TASK_PROGRESS_MIN_INTERVAL_MS: i64 = 1_000;
const MAX_TASK_DELIVERY_WATERMARKS: usize = MAX_REGISTRATIONS_TOTAL * 2;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(12);
const FCM_AUTH_RETRY_COOLDOWN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushPlatform {
    Apns,
    Fcm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushRegistrationKind {
    Application,
    #[serde(alias = "task_live_activity")]
    TaskActivity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushEnvironment {
    Sandbox,
    Production,
}

impl Default for PushEnvironment {
    fn default() -> Self {
        Self::Production
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRegistration {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
    pub platform: PushPlatform,
    pub kind: PushRegistrationKind,
    pub token: String,
    #[serde(default)]
    pub environment: PushEnvironment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RegisterPushInput {
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
    pub platform: PushPlatform,
    pub kind: PushRegistrationKind,
    pub token: String,
    pub environment: PushEnvironment,
    pub task_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum MobilePushError {
    #[error("push token is invalid")]
    InvalidToken,
    #[error("push registration binding is invalid")]
    InvalidBinding,
    #[error("task activity requires a task id")]
    TaskRequired,
    #[error("application registration cannot carry a task id")]
    UnexpectedTask,
    #[error("too many push registrations")]
    Capacity,
    #[error("mobile push store: {0}")]
    Io(#[from] std::io::Error),
    #[error("mobile push store is unreadable: {0}")]
    Corrupt(String),
}

pub struct MobilePushStore {
    path: PathBuf,
    registrations: RwLock<HashMap<String, PushRegistration>>,
    write_lock: Mutex<()>,
}

#[derive(Clone)]
struct TokenRemoval {
    id: String,
    token: String,
    updated_at_ms: Option<i64>,
}

impl MobilePushStore {
    pub async fn open(base_root: &Path) -> Result<Self, MobilePushError> {
        let path = base_root
            .join("system")
            .join("mobile-push-registrations.json");
        let (entries, existed) = match tokio::fs::read(&path).await {
            Ok(bytes) => (
                serde_json::from_slice::<Vec<PushRegistration>>(&bytes)
                    .map_err(|error| MobilePushError::Corrupt(error.to_string()))?,
                true,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), false),
            Err(error) => return Err(error.into()),
        };
        if entries.len() > MAX_REGISTRATIONS_TOTAL {
            return Err(MobilePushError::Capacity);
        }
        let mut registrations = HashMap::with_capacity(entries.len());
        let mut device_counts = HashMap::<(String, String, String), usize>::new();
        for row in entries {
            validate_input(&RegisterPushInput {
                principal: row.principal.clone(),
                workspace: row.workspace.clone(),
                device_id: row.device_id.clone(),
                platform: row.platform,
                kind: row.kind,
                token: row.token.clone(),
                environment: row.environment,
                task_id: row.task_id.clone(),
                updated_at_ms: row.updated_at_ms,
            })?;
            let expected_id = registration_id(
                &row.principal,
                &row.workspace,
                &row.device_id,
                row.platform,
                row.kind,
                row.task_id.as_deref(),
            );
            if row.id != expected_id || registrations.insert(row.id.clone(), row.clone()).is_some()
            {
                return Err(MobilePushError::Corrupt(
                    "duplicate or mismatched registration id".to_string(),
                ));
            }
            let count = device_counts
                .entry((row.principal, row.workspace, row.device_id))
                .or_default();
            *count += 1;
            if *count > MAX_REGISTRATIONS_PER_DEVICE {
                return Err(MobilePushError::Capacity);
            }
        }
        #[cfg(unix)]
        if existed {
            use std::os::unix::fs::PermissionsExt as _;

            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        #[cfg(not(unix))]
        let _ = existed;
        Ok(Self {
            path,
            registrations: RwLock::new(registrations),
            write_lock: Mutex::new(()),
        })
    }

    pub async fn upsert(
        &self,
        input: RegisterPushInput,
    ) -> Result<PushRegistration, MobilePushError> {
        validate_input(&input)?;
        let id = registration_id(
            &input.principal,
            &input.workspace,
            &input.device_id,
            input.platform,
            input.kind,
            input.task_id.as_deref(),
        );
        let row = PushRegistration {
            id: id.clone(),
            principal: input.principal,
            workspace: input.workspace,
            device_id: input.device_id,
            platform: input.platform,
            kind: input.kind,
            token: input.token,
            environment: input.environment,
            task_id: input.task_id,
            updated_at_ms: input.updated_at_ms,
        };
        self.commit(move |rows| {
            let mut row = row;
            if let Some(previous) = rows.get(&id) {
                // `updated_at_ms` is also the compare/delete generation. Two
                // route uploads can land in one wall-clock millisecond, so
                // make replacement revisions strictly monotonic per row.
                row.updated_at_ms = row
                    .updated_at_ms
                    .max(previous.updated_at_ms.saturating_add(1));
            }
            let device_count = rows
                .values()
                .filter(|candidate| {
                    candidate.principal == row.principal
                        && candidate.workspace == row.workspace
                        && candidate.device_id == row.device_id
                        && candidate.id != id
                })
                .count();
            if device_count >= MAX_REGISTRATIONS_PER_DEVICE && !rows.contains_key(&id) {
                // ActivityKit tokens are ephemeral. If an app was suspended
                // before its watchdog could unregister an abandoned activity,
                // let a newer route deterministically replace the oldest task
                // route rather than permanently bricking push registration for
                // this device. The durable application route is never evicted.
                let oldest_task_id = rows
                    .values()
                    .filter(|candidate| {
                        candidate.principal == row.principal
                            && candidate.workspace == row.workspace
                            && candidate.device_id == row.device_id
                            && candidate.kind == PushRegistrationKind::TaskActivity
                    })
                    .min_by_key(|candidate| candidate.updated_at_ms)
                    .map(|candidate| candidate.id.clone());
                if let Some(oldest_task_id) = oldest_task_id {
                    rows.remove(&oldest_task_id);
                } else {
                    return Err(MobilePushError::Capacity);
                }
            }
            if rows.len() >= MAX_REGISTRATIONS_TOTAL && !rows.contains_key(&id) {
                return Err(MobilePushError::Capacity);
            }
            rows.insert(id, row.clone());
            Ok(row)
        })
        .await
    }

    pub async fn remove_for_device_kind(
        &self,
        principal: &str,
        workspace: &str,
        device_id: &str,
        kind: PushRegistrationKind,
        task_id: Option<&str>,
        expected_revision: Option<i64>,
    ) -> Result<usize, MobilePushError> {
        self.commit(|rows| {
            let before = rows.len();
            rows.retain(|_, row| {
                !(row.principal == principal
                    && row.workspace == workspace
                    && row.device_id == device_id
                    && row.kind == kind
                    && row.task_id.as_deref() == task_id
                    && expected_revision.is_none_or(|revision| row.updated_at_ms == revision))
            });
            Ok(before - rows.len())
        })
        .await
    }

    pub async fn remove_for_device(
        &self,
        principal: &str,
        workspace: &str,
        device_id: &str,
    ) -> Result<usize, MobilePushError> {
        self.commit(|rows| {
            let before = rows.len();
            rows.retain(|_, row| {
                !(row.principal == principal
                    && row.workspace == workspace
                    && row.device_id == device_id)
            });
            Ok(before - rows.len())
        })
        .await
    }

    pub async fn application_registrations(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<PushRegistration> {
        self.registrations
            .read()
            .await
            .values()
            .filter(|row| {
                row.principal == principal
                    && row.workspace == workspace
                    && row.kind == PushRegistrationKind::Application
            })
            .cloned()
            .collect()
    }

    pub async fn task_registrations(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
    ) -> Vec<PushRegistration> {
        self.registrations
            .read()
            .await
            .values()
            .filter(|row| {
                row.principal == principal
                    && row.workspace == workspace
                    && row.kind == PushRegistrationKind::TaskActivity
                    && row.task_id.as_deref() == Some(task_id)
            })
            .cloned()
            .collect()
    }

    async fn remove_matching_tokens(&self, matches: Vec<TokenRemoval>) {
        if matches.is_empty() {
            return;
        }
        let matches = matches
            .into_iter()
            .map(|candidate| (candidate.id.clone(), candidate))
            .collect::<HashMap<_, _>>();
        if let Err(error) = self
            .commit(move |rows| {
                let before = rows.len();
                // A provider reply belongs to the exact token sent. The phone
                // may have replaced the same logical route while that request
                // was in flight; never let an old invalidation erase its new
                // token merely because both share a deterministic row id.
                rows.retain(|id, row| {
                    matches.get(id).is_none_or(|matched| {
                        matched.token != row.token
                            || matched
                                .updated_at_ms
                                .is_some_and(|revision| revision != row.updated_at_ms)
                    })
                });
                Ok(before - rows.len())
            })
            .await
        {
            warn!(%error, "[MOBILE-PUSH] could not persist compare-and-delete token removal");
        }
    }

    /// Apply one mutation to a private copy, make that copy durable, and only
    /// then publish it to readers. Every mutation goes through this boundary,
    /// so a failed write cannot leave memory claiming a registration exists
    /// when the API told the phone it did not.
    async fn commit<T>(
        &self,
        mutate: impl FnOnce(&mut HashMap<String, PushRegistration>) -> Result<T, MobilePushError>,
    ) -> Result<T, MobilePushError> {
        let _guard = self.write_lock.lock().await;
        let mut next = self.registrations.read().await.clone();
        let output = mutate(&mut next)?;
        self.persist_snapshot(&next).await?;
        *self.registrations.write().await = next;
        Ok(output)
    }

    async fn persist_snapshot(
        &self,
        rows: &HashMap<String, PushRegistration>,
    ) -> Result<(), MobilePushError> {
        let mut values = rows.values().cloned().collect::<Vec<_>>();
        values.sort_by(|left, right| left.id.cmp(&right.id));
        let bytes = serde_json::to_vec_pretty(&values)
            .map_err(|error| MobilePushError::Corrupt(error.to_string()))?;
        // This is a private token-routing store. The shared durable writer
        // creates a unique staging file, applies the private mode before the
        // rename, fsyncs the contents, publishes atomically, and fsyncs the
        // parent directory. Keeping that sequence centralized also makes this
        // store obey the repository durability ratchet.
        write_bytes_durably_with_mode(&self.path, &bytes, Some(0o600)).await?;
        Ok(())
    }
}

fn validate_input(input: &RegisterPushInput) -> Result<(), MobilePushError> {
    if [&input.principal, &input.workspace, &input.device_id]
        .into_iter()
        .any(|value| invalid_binding(value))
        || input.task_id.as_deref().is_some_and(invalid_binding)
    {
        return Err(MobilePushError::InvalidBinding);
    }
    let token = input.token.as_bytes();
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || token
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(MobilePushError::InvalidToken);
    }
    // Apple treats both device and Live Activity tokens as opaque,
    // variable-length byte sequences. Clients hex-encode those bytes for this
    // JSON boundary; validate the encoding without freezing today's token size.
    if input.platform == PushPlatform::Apns
        && (token.len() < 32
            || token.len() > 512
            || token.len() % 2 != 0
            || !token.iter().all(u8::is_ascii_hexdigit))
    {
        return Err(MobilePushError::InvalidToken);
    }
    match input.kind {
        PushRegistrationKind::Application if input.task_id.is_some() => {
            Err(MobilePushError::UnexpectedTask)
        },
        PushRegistrationKind::TaskActivity
            if input.task_id.as_deref().is_none_or(str::is_empty) =>
        {
            Err(MobilePushError::TaskRequired)
        },
        _ => Ok(()),
    }
}

fn invalid_binding(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || value.len() > MAX_BINDING_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
}

fn registration_id(
    principal: &str,
    workspace: &str,
    device_id: &str,
    platform: PushPlatform,
    kind: PushRegistrationKind,
    task_id: Option<&str>,
) -> String {
    let material = format!(
        "{principal}\0{workspace}\0{device_id}\0{platform:?}\0{kind:?}\0{}",
        task_id.unwrap_or("")
    );
    let digest = blake3::hash(material.as_bytes()).to_hex().to_string();
    format!("push_{}", &digest[..24])
}

#[derive(Clone)]
struct ApnsConfig {
    key_id: String,
    team_id: String,
    topic: String,
    key: Arc<EncodingKey>,
}

#[derive(Clone)]
struct FcmConfig {
    project_id: String,
    client_email: String,
    key: Arc<EncodingKey>,
}

#[derive(Clone, Default)]
pub struct MobilePushConfig {
    apns: Option<ApnsConfig>,
    fcm: Option<FcmConfig>,
}

#[derive(Deserialize)]
struct GoogleServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
}

impl MobilePushConfig {
    pub fn from_env() -> Self {
        let value = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let apns = value("MAGICIAN_APNS_KEY_ID")
            .zip(value("MAGICIAN_APNS_TEAM_ID"))
            .zip(value("MAGICIAN_APNS_PRIVATE_KEY_PATH"))
            .and_then(|((key_id, team_id), path)| {
                let pem = std::fs::read(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|pem| EncodingKey::from_ec_pem(&pem).map_err(anyhow::Error::from));
                match pem {
                    Ok(key) => Some(ApnsConfig {
                        key_id,
                        team_id,
                        topic: value("MAGICIAN_APNS_TOPIC")
                            .unwrap_or_else(|| "com.magicbeans100x.magican".to_string()),
                        key: Arc::new(key),
                    }),
                    Err(error) => {
                        warn!(%error, "[MOBILE-PUSH] APNs key could not be loaded");
                        None
                    },
                }
            });
        let fcm = value("MAGICIAN_FCM_SERVICE_ACCOUNT_PATH").and_then(|path| {
            let account = std::fs::read(&path)
                .context("reading FCM service account")
                .and_then(|bytes| {
                    serde_json::from_slice::<GoogleServiceAccount>(&bytes)
                        .context("decoding FCM service account")
                });
            match account.and_then(|account| {
                let key = EncodingKey::from_rsa_pem(account.private_key.as_bytes())?;
                Ok(FcmConfig {
                    project_id: account.project_id,
                    client_email: account.client_email,
                    key: Arc::new(key),
                })
            }) {
                Ok(config) => Some(config),
                Err(error) => {
                    warn!(%error, "[MOBILE-PUSH] FCM service account could not be loaded");
                    None
                },
            }
        });
        info!(
            apns = apns.is_some(),
            fcm = fcm.is_some(),
            "[MOBILE-PUSH] provider readiness"
        );
        Self { apns, fcm }
    }

    pub fn any_enabled(&self) -> bool {
        self.apns.is_some() || self.fcm.is_some()
    }

    pub fn supports(&self, platform: PushPlatform) -> bool {
        match platform {
            PushPlatform::Apns => self.apns.is_some(),
            PushPlatform::Fcm => self.fcm.is_some(),
        }
    }
}

#[derive(Clone)]
pub struct MobilePushDispatcher {
    store: Arc<MobilePushStore>,
    config: MobilePushConfig,
    client: reqwest::Client,
    apns_token: Arc<Mutex<Option<CachedToken>>>,
    fcm_token: Arc<Mutex<FcmTokenCache>>,
    event_permits: Arc<Semaphore>,
    delivery_permits: Arc<Semaphore>,
    task_delivery_watermarks: Arc<Mutex<HashMap<TaskDeliveryKey, TaskDeliveryWatermark>>>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct TaskDeliveryKey {
    principal: String,
    workspace: String,
    task_id: String,
    execution_id: String,
}

#[derive(Clone, Copy)]
struct TaskDeliveryWatermark {
    event_timestamp: i64,
    activitykit_timestamp: i64,
    touched_at_ms: i64,
    terminal: bool,
}

#[derive(Clone)]
struct CachedToken {
    value: String,
    expires_at: i64,
}

#[derive(Default)]
struct FcmTokenCache {
    token: Option<CachedToken>,
    retry_after: Option<Instant>,
}

impl FcmTokenCache {
    fn retry_blocked(&self, now: Instant) -> bool {
        self.retry_after.is_some_and(|deadline| deadline > now)
    }
}

impl MobilePushDispatcher {
    pub fn new(store: Arc<MobilePushStore>, config: MobilePushConfig) -> Self {
        Self {
            store,
            config,
            client: reqwest::Client::builder()
                .timeout(PROVIDER_TIMEOUT)
                .build()
                .expect("static mobile push HTTP client"),
            apns_token: Arc::new(Mutex::new(None)),
            fcm_token: Arc::new(Mutex::new(FcmTokenCache::default())),
            event_permits: Arc::new(Semaphore::new(EVENT_CONCURRENCY)),
            delivery_permits: Arc::new(Semaphore::new(DELIVERY_CONCURRENCY)),
            task_delivery_watermarks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn start(self: Arc<Self>, broadcaster: Arc<RuntimeTransportBroadcaster>) {
        let mut events = broadcaster.subscribe();
        tokio::spawn(async move {
            loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(count, "[MOBILE-PUSH] event subscriber lagged; clients will reconcile from Today");
                        continue;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if !is_push_event(&event) {
                    continue;
                }
                let Ok(permit) = Arc::clone(&self.event_permits).acquire_owned().await else {
                    break;
                };
                let dispatcher = Arc::clone(&self);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = dispatcher.dispatch(event).await {
                        warn!(%error, "[MOBILE-PUSH] event delivery failed");
                    }
                });
            }
        });
    }

    /// The attention push for one HITL request (P5: driven by the delivery
    /// coordinator, which owns quiet hours, dedup and the record; the
    /// payload is the same value-free card as before).
    pub async fn deliver_attention_requested(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    ) -> (usize, usize, usize) {
        let registrations = self
            .store
            .application_registrations(principal, workspace)
            .await;
        let addressed = registrations.len();
        let data = json!({"kind":"attention_requested", "correlation_id":correlation_id, "event_timestamp":timestamp, "deep_link":attention_deep_link(correlation_id)});
        let (_retry, accepted, not_configured) =
            self.deliver_many_counted(registrations, data, true).await;
        (addressed, accepted, not_configured)
    }

    pub async fn deliver_attention_resolved(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    ) {
        let registrations = self
            .store
            .application_registrations(principal, workspace)
            .await;
        let _ = self
            .deliver_many(
                registrations,
                json!({"kind":"attention_resolved", "correlation_id":correlation_id, "event_timestamp":timestamp}),
                false,
            )
            .await;
    }

    async fn dispatch(&self, event: RuntimeTransportEvent) -> Result<()> {
        match event {
            RuntimeTransportEvent::ExecutionPanelDelta {
                principal,
                workspace,
                task_id: Some(task_id),
                execution_id,
                state,
                timestamp,
                ..
            } => {
                let status = serde_json::to_value(&state.overview.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "running".to_string());
                let done = state.overview.status.is_terminal();
                let (feed, latest) = if state.run.activity_log.is_empty() {
                    (
                        &state.run.recent_activity,
                        state.run.recent_activity.first(),
                    )
                } else {
                    (&state.run.activity_log, state.run.activity_log.last())
                };
                let latest_line = latest
                    .and_then(|item| {
                        let title = item.title.trim();
                        if !title.is_empty() {
                            Some(title)
                        } else {
                            item.summary
                                .as_deref()
                                .map(str::trim)
                                .filter(|value| !value.is_empty())
                        }
                    })
                    .map(str::to_string);
                let line = task_progress_status(&status, latest_line.as_deref());
                let mut payload = json!({
                    "kind":"task_progress", "task_id":task_id,
                    "title":bounded(&state.overview.title, 96), "status":bounded(&line, 120),
                    "state":status, "step_count":feed.len(), "done":done,
                    "event_timestamp":timestamp,
                    "deep_link":task_deep_link(&task_id)
                });
                let registrations = self
                    .store
                    .task_registrations(&principal, &workspace, &task_id)
                    .await;
                if registrations.is_empty() {
                    return Ok(());
                }
                let delivery_key = TaskDeliveryKey {
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    task_id: task_id.clone(),
                    execution_id: execution_id
                        .or_else(|| state.overview.execution_id.clone())
                        .unwrap_or_else(|| task_id.clone()),
                };
                let Some(activitykit_timestamp) = self
                    .admit_task_progress(delivery_key, timestamp, done)
                    .await
                else {
                    return Ok(());
                };
                payload["activitykit_timestamp"] = Value::Number(activitykit_timestamp.into());
                if done {
                    let terminal_routes = registrations
                        .iter()
                        .map(|registration| TokenRemoval {
                            id: registration.id.clone(),
                            token: registration.token.clone(),
                            updated_at_ms: Some(registration.updated_at_ms),
                        })
                        .collect();
                    self.deliver_terminal(registrations, payload).await;
                    // Delete exactly the routes that received this terminal
                    // wave. A replacement token may have been uploaded under
                    // the same deterministic task row while provider delivery
                    // was in flight; compare-and-delete preserves it.
                    self.store.remove_matching_tokens(terminal_routes).await;
                } else {
                    let _ = self.deliver_many(registrations, payload, false).await;
                }
            },
            _ => {},
        }
        Ok(())
    }

    /// Remote task cards are glance surfaces, not token streams. Admit at most
    /// one non-terminal update per second for each execution, while terminal
    /// state always wins immediately and permanently fences late progress for
    /// that execution. The bounded journal prevents abandoned runs from
    /// accumulating process memory forever.
    async fn admit_task_progress(
        &self,
        key: TaskDeliveryKey,
        event_timestamp: i64,
        terminal: bool,
    ) -> Option<i64> {
        let mut watermarks = self.task_delivery_watermarks.lock().await;
        if let Some(previous) = watermarks.get(&key) {
            if previous.terminal
                || (!terminal
                    && (event_timestamp <= previous.event_timestamp
                        || event_timestamp.saturating_sub(previous.event_timestamp)
                            < TASK_PROGRESS_MIN_INTERVAL_MS))
            {
                return None;
            }
        }
        // ActivityKit's wire timestamp is seconds, while the canonical event
        // clock is milliseconds. A terminal event may legitimately bypass the
        // one-second progress rate and land in the same wall-clock second as a
        // preceding low-priority update. Give every admitted update a strictly
        // increasing ActivityKit timestamp so the delayed low-priority packet
        // cannot render after (or obscure) terminal truth. Android continues to
        // receive the untouched canonical millisecond timestamp.
        let activitykit_timestamp = event_timestamp.div_euclid(1_000).max(
            watermarks
                .get(&key)
                .map(|previous| previous.activitykit_timestamp.saturating_add(1))
                .unwrap_or(i64::MIN),
        );
        if !watermarks.contains_key(&key) && watermarks.len() >= MAX_TASK_DELIVERY_WATERMARKS {
            if let Some(oldest) = watermarks
                .iter()
                .min_by_key(|(_, value)| value.touched_at_ms)
                .map(|(key, _)| key.clone())
            {
                watermarks.remove(&oldest);
            }
        }
        watermarks.insert(
            key,
            TaskDeliveryWatermark {
                event_timestamp,
                activitykit_timestamp,
                touched_at_ms: chrono::Utc::now().timestamp_millis(),
                terminal,
            },
        );
        Some(activitykit_timestamp)
    }

    /// The registrations worth another attempt, and how many the provider
    /// actually took.
    ///
    /// A token the provider REJECTED is neither retryable nor delivered: it is
    /// removed from the store, and counting it as accepted (which
    /// `addressed - retry.len()` did) turned "every device token is stale" into
    /// a wave reported `provider_accepted` that reached nobody — the one thing
    /// the delivery contract says never to claim.
    async fn deliver_many_counted(
        &self,
        registrations: Vec<PushRegistration>,
        payload: Value,
        alert: bool,
    ) -> (Vec<PushRegistration>, usize, usize) {
        let outcomes = stream::iter(registrations)
            .map(|registration| {
                let payload = payload.clone();
                async move {
                    let result = self.deliver_one(&registration, &payload, alert).await;
                    (registration, result)
                }
            })
            .buffer_unordered(DELIVERY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        let mut retry = Vec::new();
        let mut invalidations = Vec::new();
        let mut accepted = 0usize;
        let mut not_configured = 0usize;
        for (registration, result) in outcomes {
            match result {
                Ok(DeliveryDisposition::Accepted) => accepted += 1,
                // Nothing was sent and nothing is retryable: the platform has
                // no provider here. Counting it as accepted would report a
                // delivery that never left; counting it as a failure would
                // burn three attempts and land `failed` on every host without
                // push credentials. It is counted as itself.
                Ok(DeliveryDisposition::NotConfigured) => not_configured += 1,
                Ok(DeliveryDisposition::InvalidToken) => invalidations.push(TokenRemoval {
                    id: registration.id,
                    token: registration.token,
                    updated_at_ms: None,
                }),
                Err(error) => {
                    warn!(platform=?registration.platform, kind=?registration.kind, %error, "[MOBILE-PUSH] provider request failed");
                    retry.push(registration);
                },
            }
        }
        self.store.remove_matching_tokens(invalidations).await;
        (retry, accepted, not_configured)
    }

    /// The registrations worth another attempt, for callers that do not report
    /// an acceptance count.
    async fn deliver_many(
        &self,
        registrations: Vec<PushRegistration>,
        payload: Value,
        alert: bool,
    ) -> Vec<PushRegistration> {
        self.deliver_many_counted(registrations, payload, alert)
            .await
            .0
    }

    async fn deliver_one(
        &self,
        registration: &PushRegistration,
        payload: &Value,
        alert: bool,
    ) -> Result<DeliveryDisposition> {
        let _permit = Arc::clone(&self.delivery_permits)
            .acquire_owned()
            .await
            .context("mobile push delivery semaphore closed")?;
        match registration.platform {
            PushPlatform::Apns => self.send_apns(registration, payload, alert).await,
            PushPlatform::Fcm => self.send_fcm(registration, payload, alert).await,
        }
    }

    async fn deliver_terminal(&self, registrations: Vec<PushRegistration>, payload: Value) {
        let mut pending = registrations;
        for delay in [0_u64, 1, 3] {
            if pending.is_empty() {
                return;
            }
            if delay > 0 {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            pending = self.deliver_many(pending, payload.clone(), false).await;
        }
        if !pending.is_empty() {
            warn!(
                count = pending.len(),
                "[MOBILE-PUSH] terminal task update exhausted bounded retries"
            );
        }
    }

    async fn send_apns(
        &self,
        registration: &PushRegistration,
        data: &Value,
        alert: bool,
    ) -> Result<DeliveryDisposition> {
        let Some(config) = self.config.apns.as_ref() else {
            return Ok(DeliveryDisposition::NotConfigured);
        };
        let auth = self.apns_bearer(config).await?;
        let live = registration.kind == PushRegistrationKind::TaskActivity
            && registration.platform == PushPlatform::Apns;
        let terminal = data.get("done").and_then(Value::as_bool).unwrap_or(false);
        let topic = if live {
            format!("{}.push-type.liveactivity", config.topic)
        } else {
            config.topic.clone()
        };
        let body = if live {
            live_activity_payload(data)
        } else {
            application_apns_payload(data, alert)
        };
        let host = match registration.environment {
            PushEnvironment::Sandbox => "https://api.sandbox.push.apple.com",
            PushEnvironment::Production => "https://api.push.apple.com",
        };
        let response = self
            .client
            .post(format!("{host}/3/device/{}", registration.token))
            .bearer_auth(&auth)
            .header("apns-topic", topic)
            .header(
                "apns-push-type",
                if live {
                    "liveactivity"
                } else if alert {
                    "alert"
                } else {
                    "background"
                },
            )
            .header(
                "apns-priority",
                if delivery_is_urgent(alert, terminal) {
                    "10"
                } else {
                    "5"
                },
            )
            .json(&body)
            .send()
            .await
            .map_err(|error| sanitized_provider_transport_error("APNs", &error))?;
        if response.status().is_success() {
            return Ok(DeliveryDisposition::Accepted);
        }
        let status = response.status();
        let reason = response.text().await.unwrap_or_default();
        if apns_provider_bearer_was_rejected(status, &reason) {
            self.discard_apns_bearer(&auth).await;
        }
        if apns_token_is_invalid(status, &reason) {
            Ok(DeliveryDisposition::InvalidToken)
        } else {
            // Provider responses are untrusted and may echo request fields.
            // Keep routing tokens out of logs even when a provider misbehaves.
            anyhow::bail!("APNs returned {status}")
        }
    }

    async fn apns_bearer(&self, config: &ApnsConfig) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let mut cache = self.apns_token.lock().await;
        if let Some(token) = cache.as_ref().filter(|token| token.expires_at > now + 60) {
            return Ok(token.value.clone());
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            iat: i64,
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(config.key_id.clone());
        let value = jsonwebtoken::encode(
            &header,
            &Claims {
                iss: &config.team_id,
                iat: now,
            },
            &config.key,
        )?;
        *cache = Some(CachedToken {
            value: value.clone(),
            expires_at: now + 50 * 60,
        });
        Ok(value)
    }

    async fn discard_apns_bearer(&self, rejected: &str) {
        let mut cache = self.apns_token.lock().await;
        if cache.as_ref().is_some_and(|token| token.value == rejected) {
            *cache = None;
        }
    }

    async fn send_fcm(
        &self,
        registration: &PushRegistration,
        data: &Value,
        alert: bool,
    ) -> Result<DeliveryDisposition> {
        let Some(config) = self.config.fcm.as_ref() else {
            return Ok(DeliveryDisposition::NotConfigured);
        };
        let auth = self.fcm_bearer(config).await?;
        let mut fields = HashMap::<String, String>::new();
        if let Some(object) = data.as_object() {
            for (key, value) in object {
                fields.insert(
                    key.clone(),
                    value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
        }
        if alert {
            fields.insert("notify".to_string(), "true".to_string());
        }
        let terminal = data.get("done").and_then(Value::as_bool).unwrap_or(false);
        let response = self.client.post(format!("https://fcm.googleapis.com/v1/projects/{}/messages:send", config.project_id))
            .bearer_auth(&auth)
            .json(&json!({"message":{"token":registration.token,"data":fields,"android":{"priority":if delivery_is_urgent(alert, terminal) {"high"} else {"normal"}}}}))
            .send().await?;
        if response.status().is_success() {
            return Ok(DeliveryDisposition::Accepted);
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if fcm_provider_bearer_was_rejected(status) {
            self.discard_fcm_bearer(&auth).await;
        }
        if fcm_token_is_unregistered(&body) {
            Ok(DeliveryDisposition::InvalidToken)
        } else {
            // FCM error bodies can contain field-level request diagnostics.
            // Never forward one into the application error/logging surface.
            anyhow::bail!("FCM returned {status}")
        }
    }

    async fn fcm_bearer(&self, config: &FcmConfig) -> Result<String> {
        let now = chrono::Utc::now().timestamp();
        let mut cache = self.fcm_token.lock().await;
        if let Some(token) = cache
            .token
            .as_ref()
            .filter(|token| token.expires_at > now + 60)
        {
            return Ok(token.value.clone());
        }
        if cache.retry_blocked(Instant::now()) {
            anyhow::bail!("FCM OAuth refresh is in bounded retry cooldown");
        }
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            iat: i64,
            exp: i64,
        }
        let assertion = jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                iss: &config.client_email,
                scope: "https://www.googleapis.com/auth/firebase.messaging",
                aud: "https://oauth2.googleapis.com/token",
                iat: now,
                exp: now + 3_600,
            },
            &config.key,
        )?;
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: i64,
        }
        let response = async {
            self.client
                .post("https://oauth2.googleapis.com/token")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", assertion.as_str()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<TokenResponse>()
                .await
        }
        .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                cache.retry_after = Some(Instant::now() + FCM_AUTH_RETRY_COOLDOWN);
                return Err(error.into());
            },
        };
        let expires_at = now + response.expires_in.max(60);
        cache.token = Some(CachedToken {
            value: response.access_token.clone(),
            expires_at,
        });
        cache.retry_after = None;
        Ok(response.access_token)
    }

    async fn discard_fcm_bearer(&self, rejected: &str) {
        let mut cache = self.fcm_token.lock().await;
        if cache
            .token
            .as_ref()
            .is_some_and(|token| token.value == rejected)
        {
            cache.token = None;
            cache.retry_after = Some(Instant::now() + FCM_AUTH_RETRY_COOLDOWN);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryDisposition {
    Accepted,
    InvalidToken,
    /// This platform has no provider configured on this runtime, so nothing was
    /// sent. Distinct from `Accepted`: a wave of these must not be reported as
    /// a delivery the provider took (an iOS registration on an FCM-only runtime
    /// used to count as accepted).
    NotConfigured,
}

/// The events the dispatcher handles on its own subscription. HITL
/// lifecycle pushes are driven by the delivery coordinator since P5
/// (`hitl_delivery`), which applies one policy — quiet hours, dedup, the
/// delivery record — to every destination class; the app-owner-notification
/// exclusion (app.notify.v1) is applied there too.
fn is_push_event(event: &RuntimeTransportEvent) -> bool {
    matches!(
        event,
        RuntimeTransportEvent::ExecutionPanelDelta {
            task_id: Some(_),
            ..
        }
    )
}

#[async_trait::async_trait]
impl crate::magician_v2::hitl_delivery::PushSink for MobilePushDispatcher {
    async fn attention_requested(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    ) -> crate::magician_v2::hitl_delivery::PushWave {
        let (registrations, accepted, not_configured) = self
            .deliver_attention_requested(principal, workspace, correlation_id, timestamp)
            .await;
        crate::magician_v2::hitl_delivery::PushWave {
            registrations,
            accepted,
            not_configured,
        }
    }

    async fn attention_resolved(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        timestamp: i64,
    ) {
        self.deliver_attention_resolved(principal, workspace, correlation_id, timestamp)
            .await;
    }
}

fn application_apns_payload(data: &Value, alert: bool) -> Value {
    let mut aps = serde_json::Map::new();
    // A visible Needs You alert must also refresh the at-a-glance projection.
    // Without this flag iOS may display the banner while leaving the Home
    // Screen widget stale until its next WidgetKit poll or app foreground.
    aps.insert("content-available".to_string(), Value::Number(1.into()));
    if alert {
        aps.insert("alert".to_string(), json!({"title":"Magican needs your input", "body":"Open Attention to keep your work moving."}));
        aps.insert("sound".to_string(), Value::String("default".to_string()));
        aps.insert(
            "thread-id".to_string(),
            Value::String("magican-attention".to_string()),
        );
    }
    let mut root = data.as_object().cloned().unwrap_or_default();
    root.insert("aps".to_string(), Value::Object(aps));
    Value::Object(root)
}

fn live_activity_payload(data: &Value) -> Value {
    let done = data.get("done").and_then(Value::as_bool).unwrap_or(false);
    let timestamp = data
        .get("activitykit_timestamp")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .or_else(|| {
            data.get("event_timestamp")
                .and_then(Value::as_i64)
                .filter(|value| *value > 0)
                .map(|value| value / 1_000)
        })
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut aps = json!({
        "timestamp": timestamp,
        "event": if done { "end" } else { "update" },
        "content-state": {
            "status": data.get("status").and_then(Value::as_str).unwrap_or("Working…"),
            "isDone": done,
            "stepCount": data.get("step_count").and_then(Value::as_u64).unwrap_or(0),
            "cardTitle": data.get("title").and_then(Value::as_str).unwrap_or("")
        }
    });
    if done {
        aps["dismissal-date"] = Value::Number((timestamp + 60).into());
    } else {
        aps["stale-date"] = Value::Number((timestamp + 20 * 60).into());
    }
    json!({"aps":aps})
}

fn bounded(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn task_progress_status(status: &str, latest_activity: Option<&str>) -> String {
    match status {
        "completed" => "Done.".to_string(),
        "failed" => "Did not finish.".to_string(),
        "cancelled" => "Cancelled.".to_string(),
        _ => latest_activity.unwrap_or("Working…").to_string(),
    }
}

fn delivery_is_urgent(alert: bool, terminal: bool) -> bool {
    alert || terminal
}

fn sanitized_provider_transport_error(provider: &str, error: &reqwest::Error) -> anyhow::Error {
    // In APNs the device token is a URL path component, and reqwest's Display
    // includes the request URL. Preserve the actionable class, never the raw
    // error/source chain.
    let class = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_request() {
        "request construction failed"
    } else {
        "transport failed"
    };
    anyhow::anyhow!("{provider} {class}")
}

fn apns_token_is_invalid(status: StatusCode, body: &str) -> bool {
    if status == StatusCode::GONE {
        return true;
    }
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|response| {
            response
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|reason| matches!(reason.as_str(), "BadDeviceToken" | "Unregistered"))
}

fn apns_provider_bearer_was_rejected(status: StatusCode, body: &str) -> bool {
    status == StatusCode::FORBIDDEN
        && serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|response| {
                response
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|reason| reason == "ExpiredProviderToken")
}

fn fcm_provider_bearer_was_rejected(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED
}

fn fcm_token_is_unregistered(body: &str) -> bool {
    let Ok(response) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    response
        .pointer("/error/details")
        .and_then(Value::as_array)
        .is_some_and(|details| {
            details.iter().any(|detail| {
                detail.get("errorCode").and_then(Value::as_str) == Some("UNREGISTERED")
            })
        })
}

fn attention_deep_link(correlation_id: &str) -> String {
    let mut url = url::Url::parse("magican://attention").expect("static attention URL");
    url.query_pairs_mut()
        .append_pair("correlation_id", correlation_id);
    url.to_string()
}

fn task_deep_link(task_id: &str) -> String {
    let mut url = url::Url::parse("magican://task/").expect("static task URL");
    url.path_segments_mut()
        .expect("task URL supports path segments")
        .push(task_id);
    url.to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn input(
        kind: PushRegistrationKind,
        platform: PushPlatform,
        task_id: Option<&str>,
    ) -> RegisterPushInput {
        RegisterPushInput {
            principal: "owner".into(),
            workspace: "default".into(),
            device_id: "phone".into(),
            platform,
            kind,
            token: if platform == PushPlatform::Apns {
                "a".repeat(64)
            } else {
                "fcm-token-1".into()
            },
            environment: PushEnvironment::Sandbox,
            task_id: task_id.map(str::to_string),
            updated_at_ms: 1,
        }
    }

    #[tokio::test]
    async fn upsert_replaces_one_device_slot_without_exposing_a_second_row() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        let mut changed = input(PushRegistrationKind::Application, PushPlatform::Apns, None);
        changed.token = "b".repeat(64);
        store.upsert(changed).await.unwrap();
        let rows = store.application_registrations("owner", "default").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token, "b".repeat(64));
        assert_eq!(rows[0].updated_at_ms, 2);
        let reopened = MobilePushStore::open(temp.path()).await.unwrap();
        assert_eq!(
            reopened
                .application_registrations("owner", "default")
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn failed_durable_write_does_not_publish_the_registration_in_memory() {
        let temp = tempfile::tempdir().unwrap();
        let blocked_parent = temp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"blocked").unwrap();
        let store = MobilePushStore {
            path: blocked_parent.join("mobile-push-registrations.json"),
            registrations: RwLock::new(HashMap::new()),
            write_lock: Mutex::new(()),
        };

        assert!(store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .is_err());
        assert!(store
            .application_registrations("owner", "default")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn a_new_activity_evicts_only_the_oldest_ephemeral_device_route_at_capacity() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        for index in 0..(MAX_REGISTRATIONS_PER_DEVICE - 1) {
            let task_id = format!("task-{index}");
            let mut route = input(
                PushRegistrationKind::TaskActivity,
                PushPlatform::Apns,
                Some(&task_id),
            );
            route.updated_at_ms = index as i64 + 10;
            store.upsert(route).await.unwrap();
        }
        let mut newest = input(
            PushRegistrationKind::TaskActivity,
            PushPlatform::Apns,
            Some("task-new"),
        );
        newest.updated_at_ms = 1_000;
        store.upsert(newest).await.unwrap();

        assert_eq!(
            store
                .application_registrations("owner", "default")
                .await
                .len(),
            1
        );
        assert!(store
            .task_registrations("owner", "default", "task-0")
            .await
            .is_empty());
        assert_eq!(
            store
                .task_registrations("owner", "default", "task-new")
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn terminal_task_cleanup_is_durable_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        store
            .upsert(input(
                PushRegistrationKind::TaskActivity,
                PushPlatform::Apns,
                Some("task-1"),
            ))
            .await
            .unwrap();

        let row = store
            .task_registrations("owner", "default", "task-1")
            .await
            .into_iter()
            .next()
            .unwrap();
        let route = vec![TokenRemoval {
            id: row.id,
            token: row.token,
            updated_at_ms: Some(row.updated_at_ms),
        }];
        store.remove_matching_tokens(route.clone()).await;
        store.remove_matching_tokens(route).await;
        let reopened = MobilePushStore::open(temp.path()).await.unwrap();
        assert!(reopened
            .task_registrations("owner", "default", "task-1")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn provider_invalidations_are_removed_in_one_durable_batch() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        let apns = store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        let fcm = store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Fcm,
                None,
            ))
            .await
            .unwrap();

        store
            .remove_matching_tokens(vec![
                TokenRemoval {
                    id: apns.id.clone(),
                    token: apns.token.clone(),
                    updated_at_ms: None,
                },
                TokenRemoval {
                    id: fcm.id.clone(),
                    token: fcm.token.clone(),
                    updated_at_ms: None,
                },
                TokenRemoval {
                    id: apns.id,
                    token: apns.token,
                    updated_at_ms: None,
                },
            ])
            .await;

        assert!(store
            .application_registrations("owner", "default")
            .await
            .is_empty());
        let reopened = MobilePushStore::open(temp.path()).await.unwrap();
        assert!(reopened
            .application_registrations("owner", "default")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn provider_http_concurrency_is_global_across_event_batches() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let registration = store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Fcm,
                None,
            ))
            .await
            .unwrap();
        let dispatcher = MobilePushDispatcher::new(store, MobilePushConfig::default());
        let mut held = Vec::new();
        for _ in 0..DELIVERY_CONCURRENCY {
            held.push(
                Arc::clone(&dispatcher.delivery_permits)
                    .acquire_owned()
                    .await
                    .unwrap(),
            );
        }
        let payload = json!({});
        let delivery = dispatcher.deliver_one(&registration, &payload, false);
        tokio::pin!(delivery);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut delivery)
                .await
                .is_err()
        );

        drop(held);
        // The permit is what this test is about. With no provider configured
        // the delivery is `NotConfigured`, not `Accepted`: nothing was sent, so
        // nothing may be counted as accepted.
        assert_eq!(delivery.await.unwrap(), DeliveryDisposition::NotConfigured);
    }

    #[tokio::test]
    async fn stale_provider_invalidation_cannot_delete_a_replacement_token() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        let original = store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        let mut replacement = input(PushRegistrationKind::Application, PushPlatform::Apns, None);
        replacement.token = "b".repeat(64);
        store.upsert(replacement).await.unwrap();

        store
            .remove_matching_tokens(vec![TokenRemoval {
                id: original.id,
                token: original.token,
                updated_at_ms: None,
            }])
            .await;

        let rows = store.application_registrations("owner", "default").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token, "b".repeat(64));
    }

    #[tokio::test]
    async fn stale_terminal_cleanup_cannot_delete_a_new_same_token_route_generation() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        let original = store
            .upsert(input(
                PushRegistrationKind::TaskActivity,
                PushPlatform::Fcm,
                Some("task-1"),
            ))
            .await
            .unwrap();
        let mut replacement = input(
            PushRegistrationKind::TaskActivity,
            PushPlatform::Fcm,
            Some("task-1"),
        );
        replacement.updated_at_ms = original.updated_at_ms + 1;
        store.upsert(replacement).await.unwrap();

        store
            .remove_matching_tokens(vec![TokenRemoval {
                id: original.id,
                token: original.token,
                updated_at_ms: Some(original.updated_at_ms),
            }])
            .await;

        let rows = store.task_registrations("owner", "default", "task-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].updated_at_ms, 2);
    }

    #[tokio::test]
    async fn stale_client_teardown_cannot_delete_a_replacement_activity_route() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        let original = store
            .upsert(input(
                PushRegistrationKind::TaskActivity,
                PushPlatform::Apns,
                Some("task-1"),
            ))
            .await
            .unwrap();
        let mut replacement = input(
            PushRegistrationKind::TaskActivity,
            PushPlatform::Apns,
            Some("task-1"),
        );
        replacement.token = "b".repeat(64);
        let replacement = store.upsert(replacement).await.unwrap();

        let removed = store
            .remove_for_device_kind(
                "owner",
                "default",
                "phone",
                PushRegistrationKind::TaskActivity,
                Some("task-1"),
                Some(original.updated_at_ms),
            )
            .await
            .unwrap();

        assert_eq!(removed, 0);
        let rows = store.task_registrations("owner", "default", "task-1").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].updated_at_ms, replacement.updated_at_ms);
        assert_eq!(rows[0].token, "b".repeat(64));
    }

    #[tokio::test]
    async fn task_progress_is_rate_bounded_but_terminal_state_always_wins() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let dispatcher = MobilePushDispatcher::new(store, MobilePushConfig::default());
        let key = TaskDeliveryKey {
            principal: "owner".into(),
            workspace: "default".into(),
            task_id: "task-1".into(),
            execution_id: "exec-1".into(),
        };

        assert_eq!(
            dispatcher
                .admit_task_progress(key.clone(), 1_000, false)
                .await,
            Some(1)
        );
        assert_eq!(
            dispatcher
                .admit_task_progress(key.clone(), 1_500, false)
                .await,
            None
        );
        assert_eq!(
            dispatcher
                .admit_task_progress(key.clone(), 1_500, true)
                .await,
            Some(2)
        );
        assert_eq!(
            dispatcher.admit_task_progress(key, 3_000, false).await,
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn registration_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        let mode = std::fs::metadata(
            temp.path()
                .join("system")
                .join("mobile-push-registrations.json"),
        )
        .unwrap()
        .permissions()
        .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn opening_an_existing_registration_store_repairs_its_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join("system")
            .join("mobile-push-registrations.json");
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Apns,
                None,
            ))
            .await
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let _reopened = MobilePushStore::open(temp.path()).await.unwrap();

        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn activity_registration_is_task_bound_on_both_mobile_transports() {
        assert!(matches!(
            validate_input(&input(
                PushRegistrationKind::TaskActivity,
                PushPlatform::Apns,
                None
            )),
            Err(MobilePushError::TaskRequired)
        ));
        assert!(validate_input(&input(
            PushRegistrationKind::TaskActivity,
            PushPlatform::Apns,
            Some("t")
        ))
        .is_ok());
        assert!(validate_input(&input(
            PushRegistrationKind::TaskActivity,
            PushPlatform::Fcm,
            Some("t")
        ))
        .is_ok());
    }

    #[test]
    fn missing_provider_configuration_is_explicit_per_platform() {
        let config = MobilePushConfig::default();
        assert!(!config.supports(PushPlatform::Apns));
        assert!(!config.supports(PushPlatform::Fcm));
    }

    #[test]
    fn task_activity_uses_one_cross_platform_wire_name_with_legacy_read_compatibility() {
        assert_eq!(
            serde_json::to_string(&PushRegistrationKind::TaskActivity).unwrap(),
            "\"task_activity\""
        );
        assert_eq!(
            serde_json::from_str::<PushRegistrationKind>("\"task_live_activity\"").unwrap(),
            PushRegistrationKind::TaskActivity
        );
    }

    #[test]
    fn apns_tokens_are_validated_as_variable_length_hex_not_one_frozen_size() {
        let mut longer = input(PushRegistrationKind::Application, PushPlatform::Apns, None);
        longer.token = "ab".repeat(40);
        assert!(validate_input(&longer).is_ok());

        longer.token.push('f');
        assert!(matches!(
            validate_input(&longer),
            Err(MobilePushError::InvalidToken)
        ));
        longer.token = "zz".repeat(40);
        assert!(matches!(
            validate_input(&longer),
            Err(MobilePushError::InvalidToken)
        ));
    }

    #[test]
    fn live_activity_payload_uses_activitykit_wire_names_and_ends_terminal_work() {
        let payload = live_activity_payload(
            &json!({"title":"Build", "status":"Done", "step_count":3, "done":true}),
        );
        assert_eq!(payload["aps"]["event"], "end");
        assert_eq!(payload["aps"]["content-state"]["isDone"], true);
        assert_eq!(payload["aps"]["content-state"]["stepCount"], 3);
        assert!(payload["aps"]["dismissal-date"].is_number());
        assert!(payload["aps"].get("stale-date").is_none());
    }

    #[test]
    fn live_activity_payload_converts_epoch_milliseconds_to_seconds_once() {
        let payload = live_activity_payload(&json!({
            "event_timestamp": 1_775_000_123_456_i64,
            "done": false
        }));

        assert_eq!(payload["aps"]["timestamp"], 1_775_000_123_i64);
        assert_eq!(payload["aps"]["stale-date"], 1_775_001_323_i64);
    }

    #[test]
    fn live_activity_payload_prefers_the_monotonic_provider_timestamp() {
        let payload = live_activity_payload(&json!({
            "event_timestamp": 1_775_000_123_456_i64,
            "activitykit_timestamp": 1_775_000_124_i64,
            "done": true
        }));

        assert_eq!(payload["aps"]["timestamp"], 1_775_000_124_i64);
        assert_eq!(payload["aps"]["dismissal-date"], 1_775_000_184_i64);
    }

    #[test]
    fn terminal_push_copy_preserves_failure_and_cancellation_truth() {
        assert_eq!(
            task_progress_status("completed", Some("last step")),
            "Done."
        );
        assert_eq!(
            task_progress_status("failed", Some("last step")),
            "Did not finish."
        );
        assert_eq!(
            task_progress_status("cancelled", Some("last step")),
            "Cancelled."
        );
        assert_eq!(
            task_progress_status("running", Some("newest step")),
            "newest step"
        );
    }

    #[test]
    fn only_visible_or_terminal_delivery_uses_immediate_provider_priority() {
        assert!(delivery_is_urgent(true, false));
        assert!(delivery_is_urgent(false, true));
        assert!(!delivery_is_urgent(false, false));
    }

    #[test]
    fn one_failed_fcm_auth_refresh_temporarily_releases_all_waiters() {
        let now = Instant::now();
        let cache = FcmTokenCache {
            token: None,
            retry_after: Some(now + FCM_AUTH_RETRY_COOLDOWN),
        };

        assert!(cache.retry_blocked(now));
        assert!(!cache.retry_blocked(now + FCM_AUTH_RETRY_COOLDOWN));
    }

    #[test]
    fn fcm_only_invalidates_a_registration_for_typed_unregistered_errors() {
        assert!(fcm_token_is_unregistered(
            r#"{"error":{"code":404,"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#
        ));
        assert!(!fcm_token_is_unregistered(
            r#"{"error":{"code":404,"message":"Requested entity was not found"}}"#
        ));
        assert!(!fcm_token_is_unregistered("UNREGISTERED"));
    }

    #[test]
    fn apns_only_invalidates_for_typed_token_failures() {
        assert!(apns_token_is_invalid(
            StatusCode::BAD_REQUEST,
            r#"{"reason":"BadDeviceToken"}"#
        ));
        assert!(apns_token_is_invalid(
            StatusCode::GONE,
            r#"{"reason":"Unregistered"}"#
        ));
        assert!(!apns_token_is_invalid(
            StatusCode::BAD_REQUEST,
            r#"{"reason":"DeviceTokenNotForTopic"}"#
        ));
        assert!(!apns_token_is_invalid(
            StatusCode::BAD_REQUEST,
            "BadDeviceToken"
        ));
    }

    #[test]
    fn only_typed_provider_auth_rejections_expire_cached_bearers() {
        assert!(apns_provider_bearer_was_rejected(
            StatusCode::FORBIDDEN,
            r#"{"reason":"ExpiredProviderToken"}"#
        ));
        assert!(!apns_provider_bearer_was_rejected(
            StatusCode::FORBIDDEN,
            r#"{"reason":"DeviceTokenNotForTopic"}"#
        ));
        assert!(fcm_provider_bearer_was_rejected(StatusCode::UNAUTHORIZED));
        assert!(!fcm_provider_bearer_was_rejected(StatusCode::FORBIDDEN));
    }

    #[test]
    fn provider_transport_diagnostics_never_repeat_a_token_bearing_url() {
        let raw = reqwest::Client::new()
            .get("https://example.invalid/3/device/secret-device-token")
            .header("x-test", "invalid\nvalue")
            .build()
            .unwrap_err();
        let safe = sanitized_provider_transport_error("APNs", &raw).to_string();

        assert!(safe.starts_with("APNs "));
        assert!(!safe.contains("secret-device-token"));
        assert!(!safe.contains("example.invalid"));
    }

    #[test]
    fn ordinary_background_payload_never_invents_a_visible_alert() {
        let payload = application_apns_payload(&json!({"kind":"attention_resolved"}), false);
        assert_eq!(payload["aps"]["content-available"], 1);
        assert!(payload["aps"].get("alert").is_none());
    }

    #[test]
    fn visible_attention_alert_also_requests_a_widget_refresh() {
        let payload = application_apns_payload(&json!({"kind":"attention_requested"}), true);

        assert!(payload["aps"].get("alert").is_some());
        assert_eq!(payload["aps"]["content-available"], 1);
    }

    #[test]
    fn deep_links_encode_untrusted_identifiers_as_one_component() {
        assert_eq!(
            attention_deep_link("ask & answer"),
            "magican://attention?correlation_id=ask+%26+answer"
        );
        assert_eq!(task_deep_link("task/one"), "magican://task/task%2Fone");
    }
}
