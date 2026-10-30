//! Device-bound mobile push registrations and bounded event delivery.
//!
//! Push tokens are routing identifiers, not mobile API credentials. They live
//! in a private store separate from the public paired-device roster and are
//! never accepted with caller-supplied scope or device identity.

use std::{
    collections::{HashMap, HashSet},
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
use crate::magician_v2::web_push::{self, WebPushKeys, WebPushVapid};

const MAX_TOKEN_BYTES: usize = 4_096;
const MAX_BINDING_BYTES: usize = 512;
const MAX_REGISTRATIONS_PER_DEVICE: usize = 16;
const MAX_REGISTRATIONS_TOTAL: usize = 2_048;
/// Browser subscriptions one scope keeps. Browsers rotate subscriptions and
/// rarely say goodbye, so past this the oldest is replaced, not refused.
const MAX_WEB_REGISTRATIONS_PER_SCOPE: usize = 16;
const MAX_WEB_LABEL_CHARS: usize = 120;
/// (subscription, ask) pairs remembered so a withdrawal reaches only a
/// browser that showed the ask.
const MAX_WEB_ANNOUNCED_ASKS: usize = 8_192;
const EVENT_CONCURRENCY: usize = 8;
const DELIVERY_CONCURRENCY: usize = 8;
const TASK_PROGRESS_MIN_INTERVAL_MS: i64 = 1_000;
const MAX_TASK_DELIVERY_WATERMARKS: usize = MAX_REGISTRATIONS_TOTAL * 2;
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(12);
const FCM_AUTH_RETRY_COOLDOWN: Duration = Duration::from_secs(5);
/// How long an ask's notification stays on the phone when the ask has no
/// deadline of its own. Long enough to be seen after a meeting, short enough
/// that unanswered asks do not pile up on the lock screen; the ask itself
/// stays open in Attention. `MAGICIAN_ATTENTION_PUSH_TTL_MINUTES` overrides.
const DEFAULT_ATTENTION_PUSH_TTL: Duration = Duration::from_secs(60 * 60);
/// Bounds for the override: a notification shorter than this is gone before
/// most people look, and a longer one is the pile-up this TTL prevents.
const MIN_ATTENTION_PUSH_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_ATTENTION_PUSH_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushPlatform {
    Apns,
    Fcm,
    /// A browser's standard Web Push subscription (RFC 8030/8291/8292): the
    /// token is the endpoint URL, the keys ride `web_keys`.
    Web,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushRegistrationKind {
    Application,
    #[serde(alias = "task_live_activity")]
    TaskActivity,
    /// The coding-runs summary: an iPhone's coding-runs Live Activity update
    /// token, or an Android device's FCM token for its ongoing notification.
    CodingRuns,
    /// An iPhone's push-to-start token for the coding-runs Live Activity, so
    /// the activity appears when a run starts with the app closed.
    CodingRunsStart,
    /// One followed work item (`task_id` carries its id, e.g. `crew:…`): an
    /// iPhone's per-item Live Activity token, or an Android device's FCM
    /// token for that item's ongoing notification.
    WorkItem,
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
    /// A `web` registration's subscription keys (`p256dh`, `auth`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_keys: Option<WebPushKeys>,
    /// A `web` registration's owner-facing name ("Chrome on MacBook").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
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
            if row.platform == PushPlatform::Web
                && row
                    .web_keys
                    .as_ref()
                    .is_none_or(|keys| web_push::validate_subscription(&row.token, keys).is_err())
            {
                return Err(MobilePushError::InvalidToken);
            }
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
            web_keys: None,
            label: None,
        };
        if row.platform == PushPlatform::Web {
            // A browser subscription carries keys; it registers through
            // `upsert_web`.
            return Err(MobilePushError::InvalidToken);
        }
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
                            && candidate.kind != PushRegistrationKind::Application
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

    /// Register (or refresh) one browser subscription for a scope. Idempotent
    /// per endpoint: the endpoint names the registration. Past
    /// `MAX_WEB_REGISTRATIONS_PER_SCOPE` the scope's oldest subscription is
    /// replaced.
    pub async fn upsert_web(
        &self,
        principal: &str,
        workspace: &str,
        endpoint: &str,
        keys: &WebPushKeys,
        label: Option<&str>,
        updated_at_ms: i64,
    ) -> Result<PushRegistration, MobilePushError> {
        let keys = web_push::validate_subscription(endpoint, keys)
            .map_err(|_| MobilePushError::InvalidToken)?;
        let label = label
            .map(|label| {
                label
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(MAX_WEB_LABEL_CHARS)
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|label| !label.is_empty());
        let device_id = web_device_id(endpoint);
        let input = RegisterPushInput {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            device_id: device_id.clone(),
            platform: PushPlatform::Web,
            kind: PushRegistrationKind::Application,
            token: endpoint.to_string(),
            environment: PushEnvironment::Production,
            task_id: None,
            updated_at_ms,
        };
        validate_input(&input)?;
        let id = registration_id(
            principal,
            workspace,
            &device_id,
            PushPlatform::Web,
            PushRegistrationKind::Application,
            None,
        );
        let row = PushRegistration {
            id: id.clone(),
            principal: input.principal,
            workspace: input.workspace,
            device_id,
            platform: PushPlatform::Web,
            kind: PushRegistrationKind::Application,
            token: input.token,
            environment: PushEnvironment::Production,
            task_id: None,
            updated_at_ms,
            web_keys: Some(keys),
            label,
        };
        self.commit(move |rows| {
            let mut row = row;
            if let Some(previous) = rows.get(&id) {
                row.updated_at_ms = row
                    .updated_at_ms
                    .max(previous.updated_at_ms.saturating_add(1));
            } else {
                let scope_web = |candidate: &PushRegistration| {
                    candidate.principal == row.principal
                        && candidate.workspace == row.workspace
                        && candidate.platform == PushPlatform::Web
                };
                if rows.values().filter(|r| scope_web(r)).count() >= MAX_WEB_REGISTRATIONS_PER_SCOPE
                {
                    let oldest = rows
                        .values()
                        .filter(|r| scope_web(r))
                        .min_by_key(|r| r.updated_at_ms)
                        .map(|r| r.id.clone());
                    if let Some(oldest) = oldest {
                        rows.remove(&oldest);
                    }
                }
                if rows.len() >= MAX_REGISTRATIONS_TOTAL {
                    return Err(MobilePushError::Capacity);
                }
            }
            rows.insert(id, row.clone());
            Ok(row)
        })
        .await
    }

    /// Remove a scope's browser subscription by its endpoint.
    pub async fn remove_web(
        &self,
        principal: &str,
        workspace: &str,
        endpoint: &str,
    ) -> Result<usize, MobilePushError> {
        let id = registration_id(
            principal,
            workspace,
            &web_device_id(endpoint),
            PushPlatform::Web,
            PushRegistrationKind::Application,
            None,
        );
        if !self.registrations.read().await.contains_key(&id) {
            return Ok(0);
        }
        self.commit(|rows| Ok(usize::from(rows.remove(&id).is_some())))
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

    /// Remove every registration of one scope — the workspace was deleted.
    pub async fn remove_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<usize, MobilePushError> {
        // No rewrite (and no fsync) when the scope holds nothing.
        let any = self
            .registrations
            .read()
            .await
            .values()
            .any(|row| row.principal == principal && row.workspace == workspace);
        if !any {
            return Ok(0);
        }
        self.commit(|rows| {
            let before = rows.len();
            rows.retain(|_, row| !(row.principal == principal && row.workspace == workspace));
            Ok(before - rows.len())
        })
        .await
    }

    /// Every `(principal, workspace)` holding a registration — what the boot
    /// sweep checks for deleted workspaces.
    pub async fn registered_scopes(&self) -> Vec<(String, String)> {
        let mut scopes = self
            .registrations
            .read()
            .await
            .values()
            .map(|row| (row.principal.clone(), row.workspace.clone()))
            .collect::<Vec<_>>();
        scopes.sort();
        scopes.dedup();
        scopes
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

    /// The coding-runs routes of a scope (iPhone activity and push-to-start
    /// tokens, Android FCM tokens).
    pub async fn coding_runs_registrations(
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
                    && matches!(
                        row.kind,
                        PushRegistrationKind::CodingRuns | PushRegistrationKind::CodingRunsStart
                    )
            })
            .cloned()
            .collect()
    }

    /// Scopes with at least one coding-runs route.
    pub async fn coding_runs_scopes(&self) -> Vec<(String, String)> {
        let mut scopes: Vec<(String, String)> = self
            .registrations
            .read()
            .await
            .values()
            .filter(|row| {
                matches!(
                    row.kind,
                    PushRegistrationKind::CodingRuns
                        | PushRegistrationKind::CodingRunsStart
                        | PushRegistrationKind::WorkItem
                )
            })
            .map(|row| (row.principal.clone(), row.workspace.clone()))
            .collect();
        scopes.sort();
        scopes.dedup();
        scopes
    }

    /// The followed-item routes of a scope.
    pub async fn work_item_registrations(
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
                    && row.kind == PushRegistrationKind::WorkItem
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
    if input.platform == PushPlatform::Web
        && (input.kind != PushRegistrationKind::Application
            || web_push::validate_endpoint(&input.token).is_err())
    {
        return Err(MobilePushError::InvalidToken);
    }
    if input.platform == PushPlatform::Apns
        && (token.len() < 32
            || token.len() > 512
            || token.len() % 2 != 0
            || !token.iter().all(u8::is_ascii_hexdigit))
    {
        return Err(MobilePushError::InvalidToken);
    }
    match input.kind {
        PushRegistrationKind::Application
        | PushRegistrationKind::CodingRuns
        | PushRegistrationKind::CodingRunsStart
            if input.task_id.is_some() =>
        {
            Err(MobilePushError::UnexpectedTask)
        },
        PushRegistrationKind::TaskActivity
            if input.task_id.as_deref().is_none_or(str::is_empty) =>
        {
            Err(MobilePushError::TaskRequired)
        },
        PushRegistrationKind::WorkItem
            if !input
                .task_id
                .as_deref()
                .is_some_and(crate::magician_v2::work_live::is_item_id) =>
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

/// A browser subscription's device id: its endpoint, digested. Endpoints are
/// unique per subscription, so one endpoint is one registration.
fn web_device_id(endpoint: &str) -> String {
    let digest = blake3::hash(endpoint.as_bytes()).to_hex().to_string();
    format!("web:{}", &digest[..24])
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
    /// Browser Web Push: on only when both VAPID keys load. Shared by every
    /// clone (the API handlers and the sender hold copies), so keys installed
    /// at runtime by `POST /web-push/vapid` take effect everywhere at once.
    web: Arc<std::sync::RwLock<Option<Arc<WebPushVapid>>>>,
    /// `None` = `DEFAULT_ATTENTION_PUSH_TTL`.
    attention_ttl: Option<Duration>,
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
        let attention_ttl = value("MAGICIAN_ATTENTION_PUSH_TTL_MINUTES").and_then(|minutes| {
            match minutes.parse::<u64>() {
                Ok(minutes) => Some(
                    Duration::from_secs(minutes.saturating_mul(60))
                        .clamp(MIN_ATTENTION_PUSH_TTL, MAX_ATTENTION_PUSH_TTL),
                ),
                Err(_) => {
                    warn!("[MOBILE-PUSH] MAGICIAN_ATTENTION_PUSH_TTL_MINUTES is not a whole number of minutes; using the default");
                    None
                },
            }
        });
        let web = match WebPushVapid::from_env() {
            Ok(vapid) => vapid.map(Arc::new),
            Err(error) => {
                warn!(%error, "[MOBILE-PUSH] web push VAPID keys could not be loaded");
                None
            },
        };
        info!(
            apns = apns.is_some(),
            fcm = fcm.is_some(),
            web = web.is_some(),
            "[MOBILE-PUSH] provider readiness"
        );
        Self {
            apns,
            fcm,
            web: Arc::new(std::sync::RwLock::new(web)),
            attention_ttl,
        }
    }

    fn web_vapid(&self) -> Option<Arc<WebPushVapid>> {
        self.web
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The VAPID public key browsers subscribe with, when web push is on.
    pub fn web_push_public_key(&self) -> Option<String> {
        self.web_vapid()
            .map(|vapid| vapid.public_key_b64().to_string())
    }

    /// Turn browser Web Push on (or replace its keys) for every holder of
    /// this config, without a restart.
    pub fn install_web_push(&self, vapid: WebPushVapid) {
        *self
            .web
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(vapid));
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn with_web_push(self, vapid: WebPushVapid) -> Self {
        self.install_web_push(vapid);
        self
    }

    fn attention_ttl(&self) -> Duration {
        self.attention_ttl.unwrap_or(DEFAULT_ATTENTION_PUSH_TTL)
    }

    pub fn any_enabled(&self) -> bool {
        self.apns.is_some() || self.fcm.is_some() || self.web_vapid().is_some()
    }

    pub fn supports(&self, platform: PushPlatform) -> bool {
        match platform {
            PushPlatform::Apns => self.apns.is_some(),
            PushPlatform::Fcm => self.fcm.is_some(),
            PushPlatform::Web => self.web_vapid().is_some(),
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
    /// Asks each browser subscription was shown; withdrawals go only there.
    web_announced: Arc<std::sync::Mutex<web_push::AnnouncedAsks>>,
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
            // APNs speaks HTTP/2 only. The default (native) TLS backend offers
            // no ALPN, so every APNs request failed with "received unexpected
            // message from connection"; rustls negotiates h2.
            client: reqwest::Client::builder()
                .use_rustls_tls()
                .timeout(PROVIDER_TIMEOUT)
                .build()
                .expect("static mobile push HTTP client"),
            apns_token: Arc::new(Mutex::new(None)),
            fcm_token: Arc::new(Mutex::new(FcmTokenCache::default())),
            event_permits: Arc::new(Semaphore::new(EVENT_CONCURRENCY)),
            delivery_permits: Arc::new(Semaphore::new(DELIVERY_CONCURRENCY)),
            task_delivery_watermarks: Arc::new(Mutex::new(HashMap::new())),
            web_announced: Arc::new(std::sync::Mutex::new(web_push::AnnouncedAsks::new(
                MAX_WEB_ANNOUNCED_ASKS,
            ))),
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
    ///
    /// Time-bounded: the push carries `expires_at` (epoch ms) — the ask's
    /// deadline, else now + the attention TTL. The providers drop it
    /// undelivered past that (`apns-expiration`, FCM `ttl`), Android
    /// dismisses it then (`setTimeoutAfter`), and a silent
    /// `attention_expired` at that moment takes it down on iOS, which has no
    /// timeout for a delivered notification. The ask stays open in
    /// Attention; only the phone notification goes.
    #[allow(clippy::too_many_arguments)]
    pub async fn deliver_attention_requested(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        source: &str,
        answer: crate::magician_v2::hitl_delivery::PushAnswer,
        deadline_ms: Option<i64>,
        timestamp: i64,
    ) -> (usize, usize, usize) {
        let registrations = self
            .store
            .application_registrations(principal, workspace)
            .await;
        let addressed = registrations.len();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let expires_at = attention_expires_at_ms(now_ms, deadline_ms, self.config.attention_ttl());
        let data = attention_requested_data(correlation_id, source, answer, timestamp, expires_at);
        let (_retry, accepted, not_configured) =
            self.deliver_many_counted(registrations, data, true).await;
        if accepted > 0 {
            self.withdraw_attention_at(principal, workspace, correlation_id, expires_at);
        }
        (addressed, accepted, not_configured)
    }

    /// At `expires_at`, a silent `attention_expired` to the scope's devices
    /// so iOS takes the notification down (Android's own timeout already
    /// has). In-process only: a restart drops the timer, and the app's own
    /// sweep of expired notifications (on open and on any later push)
    /// covers that.
    fn withdraw_attention_at(
        &self,
        principal: &str,
        workspace: &str,
        correlation_id: &str,
        expires_at: i64,
    ) {
        let dispatcher = self.clone();
        let (principal, workspace, correlation_id) = (
            principal.to_string(),
            workspace.to_string(),
            correlation_id.to_string(),
        );
        tokio::spawn(async move {
            let wait = expires_at - chrono::Utc::now().timestamp_millis();
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(wait as u64)).await;
            }
            let registrations = dispatcher
                .store
                .application_registrations(&principal, &workspace)
                .await;
            let _ = dispatcher
                .deliver_many(
                    registrations,
                    json!({"kind":"attention_expired", "correlation_id":correlation_id, "event_timestamp":expires_at}),
                    false,
                )
                .await;
        });
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

    /// Scopes with a coding-runs route, for the pusher's sweep.
    pub async fn coding_runs_scopes(&self) -> Vec<(String, String)> {
        self.store.coding_runs_scopes().await
    }

    /// A scope's followed-item (`work_item`) routes.
    pub async fn work_item_registrations(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<PushRegistration> {
        self.store
            .work_item_registrations(principal, workspace)
            .await
    }

    /// Send a scope's coding-runs summary to its phones: an update (or, when
    /// nothing is going, an end) to iPhone coding-runs activities, a
    /// push-to-start to each iPhone that has no activity when `start` is set,
    /// and a data message to Android devices for their ongoing notification.
    /// `only` limits the send to those registration ids (a retry). An ended
    /// activity's route is dropped once its end was delivered; the phone
    /// registers the next activity's. Returns the ids worth another attempt.
    pub async fn deliver_coding_runs(
        &self,
        principal: &str,
        workspace: &str,
        summary: &CodingRunsPush,
        only: Option<&HashSet<String>>,
    ) -> Vec<String> {
        let registrations = self
            .store
            .coding_runs_registrations(principal, workspace)
            .await;
        let is_activity = |row: &PushRegistration| {
            row.platform == PushPlatform::Apns && row.kind == PushRegistrationKind::CodingRuns
        };
        // Per device: an iPhone with an activity gets its update, one without
        // gets the push-to-start.
        let with_activity: HashSet<String> = registrations
            .iter()
            .filter(|row| is_activity(row))
            .map(|row| row.device_id.clone())
            .collect();
        let targets: Vec<PushRegistration> = registrations
            .into_iter()
            .filter(|row| only.is_none_or(|ids| ids.contains(&row.id)))
            .filter(|row| match (row.platform, row.kind) {
                (PushPlatform::Apns, PushRegistrationKind::CodingRuns) => true,
                (PushPlatform::Apns, PushRegistrationKind::CodingRunsStart) => {
                    summary.active && summary.start && !with_activity.contains(&row.device_id)
                },
                (PushPlatform::Fcm, PushRegistrationKind::CodingRuns) => true,
                _ => false,
            })
            .collect();
        if targets.is_empty() {
            return Vec::new();
        }
        let ended: Vec<TokenRemoval> = if summary.active {
            Vec::new()
        } else {
            targets
                .iter()
                .filter(|row| is_activity(row))
                .map(|row| TokenRemoval {
                    id: row.id.clone(),
                    token: row.token.clone(),
                    // The token alone: the phone may re-post the same
                    // activity's token while the end is in flight.
                    updated_at_ms: None,
                })
                .collect()
        };
        let failed: Vec<String> = self
            .deliver_many(targets, summary.data(), false)
            .await
            .into_iter()
            .map(|row| row.id)
            .collect();
        let delivered_ends: Vec<TokenRemoval> = ended
            .into_iter()
            .filter(|removal| !failed.contains(&removal.id))
            .collect();
        self.store.remove_matching_tokens(delivered_ends).await;
        failed
    }

    /// Send one followed item's update to `routes` (its `work_item`
    /// registrations). An ended item's routes are dropped once the last
    /// update reached them. Returns the registration ids that failed.
    pub async fn deliver_work_item(
        &self,
        routes: Vec<PushRegistration>,
        item: &WorkItemPush,
    ) -> Vec<String> {
        if routes.is_empty() {
            return Vec::new();
        }
        let ended: Vec<TokenRemoval> = if item.ended {
            routes
                .iter()
                .map(|row| TokenRemoval {
                    id: row.id.clone(),
                    token: row.token.clone(),
                    updated_at_ms: None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let failed: Vec<String> = self
            .deliver_many(routes, item.data(), false)
            .await
            .into_iter()
            .map(|row| row.id)
            .collect();
        let delivered_ends: Vec<TokenRemoval> = ended
            .into_iter()
            .filter(|removal| !failed.contains(&removal.id))
            .collect();
        self.store.remove_matching_tokens(delivered_ends).await;
        failed
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
            PushPlatform::Web => self.send_web(registration, payload).await,
        }
    }

    fn web_ask_was_shown(&self, registration_id: &str, correlation_id: &str) -> bool {
        self.web_announced
            .lock()
            .is_ok_and(|asks| asks.contains(registration_id, correlation_id))
    }

    /// One browser push (RFC 8030): the content-free payload, encrypted to
    /// the subscription (RFC 8291), signed for the push service (VAPID). A
    /// 404/410 means the browser dropped the subscription: it is removed like
    /// an APNs `BadDeviceToken`.
    async fn send_web(
        &self,
        registration: &PushRegistration,
        data: &Value,
    ) -> Result<DeliveryDisposition> {
        let Some(vapid) = self.config.web_vapid() else {
            return Ok(DeliveryDisposition::NotConfigured);
        };
        let Some(payload) = web_push::web_push_payload(data) else {
            // Nothing browsers are sent (task progress rides Live Activities).
            return Ok(DeliveryDisposition::NotConfigured);
        };
        let Some(keys) = registration.web_keys.as_ref() else {
            return Ok(DeliveryDisposition::InvalidToken);
        };
        let correlation_id = payload
            .get("correlation_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let announces = payload.get("kind").and_then(Value::as_str) == Some("attention_requested");
        // A browser must show something for every push. A withdrawal of an
        // ask this subscription was never shown would be a silent push, which
        // Chrome penalises, so it is not sent.
        if !announces && !self.web_ask_was_shown(&registration.id, &correlation_id) {
            return Ok(DeliveryDisposition::NotConfigured);
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let headers =
            web_push::web_push_headers(&payload, now_ms, self.config.attention_ttl().as_secs());
        let body = web_push::encrypt(keys, &serde_json::to_vec(&payload)?)?;
        let authorization = vapid.authorization(&registration.token, now_ms / 1000)?;
        let mut request = self
            .client
            .post(&registration.token)
            .header("TTL", headers.ttl.to_string())
            .header("Urgency", headers.urgency)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Authorization", authorization)
            .body(body);
        if let Some(topic) = headers.topic {
            request = request.header("Topic", topic);
        }
        let response = request
            .send()
            .await
            .map_err(|error| sanitized_provider_transport_error("web push", &error))?;
        let status = response.status();
        if status.is_success() {
            if let Ok(mut asks) = self.web_announced.lock() {
                if announces {
                    asks.record(&registration.id, &correlation_id);
                } else {
                    asks.forget(&registration.id, &correlation_id);
                }
            }
            return Ok(DeliveryDisposition::Accepted);
        }
        if web_push_subscription_is_gone(status) {
            return Ok(DeliveryDisposition::InvalidToken);
        }
        // The endpoint embeds the subscription's capability; never repeat it
        // or the service's body.
        anyhow::bail!("web push service returned {status}")
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
        let coding_runs = matches!(
            registration.kind,
            PushRegistrationKind::CodingRuns | PushRegistrationKind::CodingRunsStart
        );
        let work_item = registration.kind == PushRegistrationKind::WorkItem;
        let live =
            (registration.kind == PushRegistrationKind::TaskActivity || coding_runs || work_item)
                && registration.platform == PushPlatform::Apns;
        let terminal = data.get("done").and_then(Value::as_bool).unwrap_or(false);
        let urgent = delivery_is_urgent(alert, terminal) || data_marks_urgent(data);
        let topic = if live {
            format!("{}.push-type.liveactivity", config.topic)
        } else {
            config.topic.clone()
        };
        let body = if coding_runs {
            coding_runs_activity_payload(
                data,
                registration.kind == PushRegistrationKind::CodingRunsStart,
            )
        } else if work_item {
            work_item_activity_payload(data)
        } else if live {
            live_activity_payload(data)
        } else {
            application_apns_payload(data, alert)
        };
        let host = match registration.environment {
            PushEnvironment::Sandbox => "https://api.sandbox.push.apple.com",
            PushEnvironment::Production => "https://api.push.apple.com",
        };
        let mut request = self
            .client
            .post(format!("{host}/3/device/{}", registration.token))
            .bearer_auth(&auth)
            .header("apns-topic", topic);
        // One notification per ask: a re-sent request replaces its own banner
        // instead of stacking. The id is a digest because APNs caps it at 64
        // bytes and correlation ids are unbounded.
        if alert && !live {
            if let Some(correlation_id) = data.get("correlation_id").and_then(Value::as_str) {
                request = request.header("apns-collapse-id", apns_collapse_id(correlation_id));
            }
        }
        // A time-bounded push is dropped undelivered once it expires (a phone
        // that was off all afternoon does not wake to a stale ask).
        if let Some(expires_at) = data.get("expires_at").and_then(Value::as_i64) {
            request = request.header("apns-expiration", (expires_at / 1000).max(1).to_string());
        }
        let response = request
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
            .header("apns-priority", if urgent { "10" } else { "5" })
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
        let fields = fcm_data_fields(data, alert);
        let terminal = data.get("done").and_then(Value::as_bool).unwrap_or(false);
        let urgent = delivery_is_urgent(alert, terminal) || data_marks_urgent(data);
        let mut android = json!({"priority":if urgent {"high"} else {"normal"}});
        // A time-bounded push is dropped undelivered once it expires.
        if let Some(ttl) = data
            .get("expires_at")
            .and_then(Value::as_i64)
            .map(|expires_at| fcm_ttl(expires_at, chrono::Utc::now().timestamp_millis()))
        {
            android["ttl"] = Value::String(ttl);
        }
        let response = self
            .client
            .post(format!(
                "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                config.project_id
            ))
            .bearer_auth(&auth)
            .json(&json!({"message":{"token":registration.token,"data":fields,"android":android}}))
            .send()
            .await?;
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
        source: &str,
        answer: crate::magician_v2::hitl_delivery::PushAnswer,
        deadline_ms: Option<i64>,
        timestamp: i64,
    ) -> crate::magician_v2::hitl_delivery::PushWave {
        let (registrations, accepted, not_configured) = self
            .deliver_attention_requested(
                principal,
                workspace,
                correlation_id,
                source,
                answer,
                deadline_ms,
                timestamp,
            )
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

/// iOS notification category for a confirmation ask: Approve / Reject.
const ATTENTION_APPROVAL_CATEGORY: &str = "MAGICAN_ATTENTION_APPROVAL";

/// The value-free data of an attention push. Answer hints only, never the
/// ask's text or an option label: the phone fetches those itself
/// (`GET /feed/attention/{correlation_id}`) and posts its answer to
/// `/hitl/{correlation_id}/respond` with `source`.
/// - `actions` (`approve_reject`): the legacy one-tap buttons, unchanged for
///   app builds that predate `answer_mode`.
/// - `answer_mode` (`confirm` | `choice` | `text`), absent when the ask must
///   be opened in the app; `option_count` only with `choice`.
fn attention_requested_data(
    correlation_id: &str,
    source: &str,
    answer: crate::magician_v2::hitl_delivery::PushAnswer,
    timestamp: i64,
    expires_at: i64,
) -> Value {
    let mut data = json!({"kind":"attention_requested", "correlation_id":correlation_id, "source":source, "event_timestamp":timestamp, "expires_at":expires_at, "deep_link":attention_deep_link(correlation_id)});
    if let Some(map) = data.as_object_mut() {
        if let Some(actions) = answer.actions {
            map.insert("actions".to_string(), Value::String(actions.to_string()));
        }
        if let Some(mode) = answer.mode {
            map.insert(
                "answer_mode".to_string(),
                Value::String(mode.as_str().to_string()),
            );
            if let Some(count) = mode.option_count() {
                map.insert("option_count".to_string(), Value::Number(count.into()));
            }
        }
    }
    data
}

/// When an ask's notification expires (epoch ms): its deadline when it has
/// one sooner than the TTL, else now + the TTL. Never in the past, so a push
/// sent at the deadline still lands for the minute it is worth anything.
fn attention_expires_at_ms(now_ms: i64, deadline_ms: Option<i64>, ttl: Duration) -> i64 {
    let by_ttl = now_ms.saturating_add(ttl.as_millis().min(i64::MAX as u128) as i64);
    deadline_ms
        .map_or(by_ttl, |deadline| deadline.min(by_ttl))
        .max(now_ms + 60_000)
}

/// FCM `android.ttl` (whole seconds, `"Ns"`) for a push expiring at
/// `expires_at`; at least one second, since `0s` means "now or never".
fn fcm_ttl(expires_at: i64, now_ms: i64) -> String {
    format!("{}s", ((expires_at - now_ms) / 1000).max(1))
}

/// FCM `data` is a string map: every field as its string (a number as its
/// decimal text, e.g. `option_count: "3"`), plus `notify: "true"` for an alert.
fn fcm_data_fields(data: &Value, alert: bool) -> HashMap<String, String> {
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
    fields
}

/// `apns-collapse-id` for one ask: a short digest of its correlation id
/// (APNs allows at most 64 bytes).
fn apns_collapse_id(correlation_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(correlation_id.as_bytes());
    format!("ask-{}", &format!("{digest:x}")[..32])
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
        // An ask's alert runs the Notification Service Extension, which may
        // fetch the ask and enrich the banner; the generic title and body
        // above stay as the fallback when it cannot.
        if data.get("kind").and_then(Value::as_str) == Some("attention_requested") {
            aps.insert("mutable-content".to_string(), Value::Number(1.into()));
        }
        // The iOS category that adds Approve / Reject (registered by the app).
        if data.get("actions").and_then(Value::as_str) == Some("approve_reject") {
            aps.insert(
                "category".to_string(),
                Value::String(ATTENTION_APPROVAL_CATEGORY.to_string()),
            );
        }
    }
    let mut root = data.as_object().cloned().unwrap_or_default();
    root.insert("aps".to_string(), Value::Object(aps));
    Value::Object(root)
}

/// A scope's coding-runs summary, as the phones show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodingRunsPush {
    pub working: u32,
    pub needs_you: u32,
    pub done: u32,
    pub failed: u32,
    /// The most urgent run, e.g. "API lane · Shop".
    pub headline: String,
    /// What that run is doing or why it waits.
    pub detail: String,
    /// A run is going or waits on the owner.
    pub active: bool,
    /// Start an iPhone activity by push if none exists (the summary just
    /// became active).
    pub start: bool,
    /// Deliver at high priority: something just started or now needs the
    /// owner. Routine count and step changes are not.
    pub urgent: bool,
    pub event_timestamp: i64,
    /// The work summary's featured item and the next ones
    /// (`work_live::WorkLive`); empty from an older summary.
    pub work: WorkSummaryFields,
}

/// The "Magician at work" fields the summary adds to the coding-runs
/// payload. Older app builds ignore them and show `headline`/`detail`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WorkSummaryFields {
    pub featured_id: String,
    pub featured_kind: String,
    pub featured_state: String,
    pub featured_title: String,
    pub featured_context: String,
    pub featured_step: String,
    pub action_kind: String,
    pub action_id: String,
    pub deep_link: String,
    /// Active items beyond the featured one and `next`.
    pub more: u32,
    pub next: Vec<WorkSummaryNext>,
}

/// One of the items listed after the featured one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkSummaryNext {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub title: String,
}

/// One followed item, as its own Live Activity or ongoing notification
/// shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkItemPush {
    pub item_id: String,
    pub kind: String,
    pub state: String,
    pub title: String,
    pub context: String,
    pub step: String,
    pub spent_usd: Option<f64>,
    pub action_kind: String,
    pub action_id: String,
    pub deep_link: String,
    /// The item ended (or left the list): the last update.
    pub ended: bool,
    pub urgent: bool,
    pub event_timestamp: i64,
}

impl WorkItemPush {
    /// Same content, ignoring when and how it is sent.
    pub fn same_content(&self, other: &Self) -> bool {
        self.item_id == other.item_id
            && self.state == other.state
            && self.title == other.title
            && self.context == other.context
            && self.step == other.step
            && self.spent_usd == other.spent_usd
            && self.action_kind == other.action_kind
            && self.action_id == other.action_id
            && self.ended == other.ended
    }

    fn data(&self) -> Value {
        json!({
            "kind": "work_item",
            "item_id": self.item_id,
            "item_kind": self.kind,
            "state": self.state,
            "title": bounded(&self.title, 96),
            "context": bounded(&self.context, 64),
            "step": bounded(&self.step, 120),
            "spent_usd": self.spent_usd,
            "action_kind": self.action_kind,
            "action_id": self.action_id,
            "deep_link": self.deep_link,
            "done": self.ended,
            "urgent": self.urgent,
            "event_timestamp": self.event_timestamp,
        })
    }
}

impl CodingRunsPush {
    /// Same lock-screen content, ignoring when and how it is sent.
    pub fn same_content(&self, other: &Self) -> bool {
        self.working == other.working
            && self.needs_you == other.needs_you
            && self.done == other.done
            && self.failed == other.failed
            && self.headline == other.headline
            && self.detail == other.detail
            && self.active == other.active
            && self.work == other.work
    }

    fn data(&self) -> Value {
        json!({
            "kind": "coding_runs",
            "working": self.working,
            "needs_you": self.needs_you,
            "done_count": self.done,
            "failed": self.failed,
            "headline": bounded(&self.headline, 96),
            "detail": bounded(&self.detail, 120),
            "active": self.active,
            // `done` marks the end of the activity for delivery priority.
            "done": !self.active,
            "urgent": self.urgent,
            "event_timestamp": self.event_timestamp,
            "deep_link": if self.work.deep_link.is_empty() {
                "magican://vibe".to_string()
            } else {
                self.work.deep_link.clone()
            },
            "featured_id": self.work.featured_id,
            "featured_kind": self.work.featured_kind,
            "featured_state": self.work.featured_state,
            "featured_title": bounded(&self.work.featured_title, 96),
            "featured_context": bounded(&self.work.featured_context, 64),
            "featured_step": bounded(&self.work.featured_step, 120),
            "action_kind": self.work.action_kind,
            "action_id": self.work.action_id,
            "more": self.work.more,
            // An array here; FCM carries it as its JSON text.
            "next": self.work.next.iter().map(|next| json!({
                "id": next.id,
                "kind": next.kind,
                "state": next.state,
                "title": bounded(&next.title, 64),
            })).collect::<Vec<_>>(),
        })
    }
}

/// ActivityKit payload for `CodingRunsActivityAttributes` (iOS): `start`
/// for a push-to-start token, else `update`, or `end` when nothing is going.
/// The content-state keys are the Swift struct's own property names.
fn coding_runs_activity_payload(data: &Value, start: bool) -> Value {
    let number = |key: &str| data.get(key).and_then(Value::as_u64).unwrap_or(0);
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let active = data.get("active").and_then(Value::as_bool).unwrap_or(false);
    let timestamp = data
        .get("event_timestamp")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .map(|value| value / 1_000)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let event = if start {
        "start"
    } else if active {
        "update"
    } else {
        "end"
    };
    let mut aps = json!({
        "timestamp": timestamp,
        "event": event,
        "content-state": {
            "working": number("working"),
            "needsYou": number("needs_you"),
            "done": number("done_count"),
            "failed": number("failed"),
            "headline": text("headline"),
            "detail": text("detail"),
            "featuredKind": text("featured_kind"),
            "featuredState": text("featured_state"),
            "featuredTitle": text("featured_title"),
            "featuredContext": text("featured_context"),
            "featuredStep": text("featured_step"),
            "actionKind": text("action_kind"),
            "actionId": text("action_id"),
            "deepLink": text("deep_link"),
            "more": number("more"),
            "next": data.get("next").cloned().unwrap_or_else(|| json!([])),
        }
    });
    if start {
        aps["attributes-type"] = json!("CodingRunsActivityAttributes");
        aps["attributes"] = json!({ "title": "Magician at work" });
        let headline = text("headline");
        aps["alert"] = json!({
            "title": "Magician at work",
            "body": if headline.is_empty() { "Work started".to_string() } else { headline },
        });
    }
    if active {
        aps["stale-date"] = Value::Number((timestamp + 20 * 60).into());
    } else {
        aps["dismissal-date"] = Value::Number((timestamp + 10 * 60).into());
    }
    json!({ "aps": aps })
}

/// ActivityKit payload for `FollowedItemActivityAttributes` (iOS): `update`
/// while the item is going, `end` with its final state once it ended. The
/// activity is started on the phone, so there is no push-to-start.
fn work_item_activity_payload(data: &Value) -> Value {
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let ended = data.get("done").and_then(Value::as_bool).unwrap_or(false);
    let timestamp = data
        .get("event_timestamp")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .map(|value| value / 1_000)
        .unwrap_or_else(|| chrono::Utc::now().timestamp());
    let mut state = json!({
        "state": text("state"),
        "title": text("title"),
        "context": text("context"),
        "step": text("step"),
        "actionKind": text("action_kind"),
        "actionId": text("action_id"),
        "deepLink": text("deep_link"),
        "updatedAt": timestamp,
    });
    if let Some(spent) = data.get("spent_usd").and_then(Value::as_f64) {
        state["spentUsd"] = json!(spent);
    }
    let mut aps = json!({
        "timestamp": timestamp,
        "event": if ended { "end" } else { "update" },
        "content-state": state,
    });
    if ended {
        aps["dismissal-date"] = Value::Number((timestamp + 10 * 60).into());
    } else {
        aps["stale-date"] = Value::Number((timestamp + 20 * 60).into());
    }
    json!({ "aps": aps })
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

/// A non-alert push that still must not wait for the device to wake on its
/// own, e.g. a coding run that just started or now waits on the owner.
fn data_marks_urgent(data: &Value) -> bool {
    data.get("urgent").and_then(Value::as_bool).unwrap_or(false)
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

/// RFC 8030 §7.3: 404/410 — the subscription expired or was unsubscribed.
fn web_push_subscription_is_gone(status: StatusCode) -> bool {
    matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
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
        assert!(!config.supports(PushPlatform::Web));
        assert!(config.web_push_public_key().is_none());
        assert!(!config.any_enabled());
        let web = config.with_web_push(test_vapid());
        assert!(web.supports(PushPlatform::Web));
        assert!(web.any_enabled());
        assert_eq!(
            web.web_push_public_key().as_deref(),
            Some(TEST_VAPID_PUBLIC)
        );
    }

    // The RFC 8291 example key pair; a test identity only.
    const TEST_VAPID_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";

    fn test_vapid() -> WebPushVapid {
        WebPushVapid::new(
            TEST_VAPID_PUBLIC,
            "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw",
            "mailto:owner@example.test",
        )
        .unwrap()
    }

    fn browser_keys() -> WebPushKeys {
        WebPushKeys {
            p256dh: "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4".into(),
            auth: "BTBZMqHH6r4Tts7J_aSIgg".into(),
        }
    }

    const CHROME: &str = "https://fcm.googleapis.com/fcm/send/chrome-1";
    const FIREFOX: &str = "https://updates.push.services.mozilla.com/wpush/v2/firefox-1";

    #[tokio::test]
    async fn web_subscriptions_are_idempotent_per_endpoint_durable_and_removable() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        let first = store
            .upsert_web(
                "owner",
                "default",
                CHROME,
                &browser_keys(),
                Some("Chrome\u{7}"),
                5,
            )
            .await
            .unwrap();
        let again = store
            .upsert_web(
                "owner",
                "default",
                CHROME,
                &browser_keys(),
                Some("Chrome"),
                5,
            )
            .await
            .unwrap();
        assert_eq!(first.id, again.id, "one endpoint is one registration");
        assert!(again.updated_at_ms > first.updated_at_ms);
        assert_eq!(first.label.as_deref(), Some("Chrome"));
        assert_eq!(first.platform, PushPlatform::Web);
        assert_eq!(first.kind, PushRegistrationKind::Application);
        let firefox = store
            .upsert_web("owner", "default", FIREFOX, &browser_keys(), None, 6)
            .await
            .unwrap();
        assert_ne!(firefox.id, first.id);
        // Web registrations are application routes: every attention push of
        // the scope reaches them, beside the phones.
        store
            .upsert(input(
                PushRegistrationKind::Application,
                PushPlatform::Fcm,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(
            store
                .application_registrations("owner", "default")
                .await
                .len(),
            3
        );
        assert!(store
            .application_registrations("owner", "other")
            .await
            .is_empty());

        // Durable, keys included, and re-validated on open.
        let reopened = MobilePushStore::open(temp.path()).await.unwrap();
        let rows = reopened.application_registrations("owner", "default").await;
        let chrome = rows.iter().find(|row| row.token == CHROME).unwrap();
        assert_eq!(chrome.web_keys.as_ref(), Some(&browser_keys()));

        // Removal is by endpoint, scoped, and idempotent.
        assert_eq!(
            reopened.remove_web("owner", "other", CHROME).await.unwrap(),
            0
        );
        assert_eq!(
            reopened
                .remove_web("owner", "default", CHROME)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            reopened
                .remove_web("owner", "default", CHROME)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            reopened
                .application_registrations("owner", "default")
                .await
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn web_subscriptions_refuse_foreign_endpoints_and_bad_keys_and_cap_per_scope() {
        let temp = tempfile::tempdir().unwrap();
        let store = MobilePushStore::open(temp.path()).await.unwrap();
        assert!(matches!(
            store
                .upsert_web(
                    "owner",
                    "default",
                    "https://example.com/push",
                    &browser_keys(),
                    None,
                    1
                )
                .await,
            Err(MobilePushError::InvalidToken)
        ));
        let bad = WebPushKeys {
            p256dh: "AAAA".into(),
            auth: browser_keys().auth,
        };
        assert!(store
            .upsert_web("owner", "default", CHROME, &bad, None, 1)
            .await
            .is_err());
        // The generic upsert cannot mint a keyless web route.
        let mut keyless = input(PushRegistrationKind::Application, PushPlatform::Web, None);
        keyless.token = CHROME.into();
        assert!(store.upsert(keyless).await.is_err());

        for index in 0..(MAX_WEB_REGISTRATIONS_PER_SCOPE + 2) {
            store
                .upsert_web(
                    "owner",
                    "default",
                    &format!("{CHROME}-{index}"),
                    &browser_keys(),
                    None,
                    10 + index as i64,
                )
                .await
                .unwrap();
        }
        let rows = store.application_registrations("owner", "default").await;
        assert_eq!(rows.len(), MAX_WEB_REGISTRATIONS_PER_SCOPE);
        // The oldest were replaced.
        assert!(!rows.iter().any(|row| row.token == format!("{CHROME}-0")));
        assert!(!rows.iter().any(|row| row.token == format!("{CHROME}-1")));
        assert!(rows
            .iter()
            .any(|row| row.token == format!("{CHROME}-{}", MAX_WEB_REGISTRATIONS_PER_SCOPE + 1)));
    }

    #[tokio::test]
    async fn a_gone_browser_subscription_is_pruned_like_a_bad_device_token() {
        assert!(web_push_subscription_is_gone(StatusCode::GONE));
        assert!(web_push_subscription_is_gone(StatusCode::NOT_FOUND));
        for status in [
            StatusCode::CREATED,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(!web_push_subscription_is_gone(status), "{status}");
        }
        // The disposition a 404/410 yields removes the exact registration.
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(MobilePushStore::open(temp.path()).await.unwrap());
        let row = store
            .upsert_web("owner", "default", CHROME, &browser_keys(), None, 1)
            .await
            .unwrap();
        store
            .remove_matching_tokens(vec![TokenRemoval {
                id: row.id,
                token: row.token,
                updated_at_ms: None,
            }])
            .await;
        assert!(store
            .application_registrations("owner", "default")
            .await
            .is_empty());

        // Without VAPID keys a browser route is not configured — nothing is
        // sent, nothing counted as accepted — and a task-progress payload is
        // never sent to a browser.
        let row = store
            .upsert_web("owner", "default", FIREFOX, &browser_keys(), None, 2)
            .await
            .unwrap();
        let off = MobilePushDispatcher::new(Arc::clone(&store), MobilePushConfig::default());
        let attention = json!({"kind":"attention_requested","correlation_id":"c1","source":"agentic","expires_at":1});
        assert!(matches!(
            off.send_web(&row, &attention).await.unwrap(),
            DeliveryDisposition::NotConfigured
        ));
        let on = MobilePushDispatcher::new(
            Arc::clone(&store),
            MobilePushConfig::default().with_web_push(test_vapid()),
        );
        assert!(matches!(
            on.send_web(&row, &json!({"kind":"task_progress","task_id":"t"}))
                .await
                .unwrap(),
            DeliveryDisposition::NotConfigured
        ));
        // A withdrawal of an ask this browser was never shown is not sent: it
        // would be a silent push.
        for kind in ["attention_resolved", "attention_expired"] {
            assert!(matches!(
                on.send_web(
                    &row,
                    &json!({"kind": kind, "correlation_id": "never-shown"})
                )
                .await
                .unwrap(),
                DeliveryDisposition::NotConfigured
            ));
        }
        on.web_announced.lock().unwrap().record(&row.id, "shown");
        assert!(on.web_ask_was_shown(&row.id, "shown"));
        assert!(!on.web_ask_was_shown("push_other", "shown"));
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
    fn coding_runs_payload_matches_the_swift_content_state_and_starts_or_ends() {
        let push = CodingRunsPush {
            working: 2,
            needs_you: 1,
            done: 3,
            failed: 0,
            headline: "API lane · Shop".to_string(),
            detail: "Needs you".to_string(),
            active: true,
            start: true,
            urgent: true,
            event_timestamp: 1_760_346_000_123,
            work: WorkSummaryFields::default(),
        };
        let data = push.data();
        assert!(data_marks_urgent(&data));
        let update = coding_runs_activity_payload(&data, false);
        assert_eq!(update["aps"]["event"], "update");
        assert_eq!(update["aps"]["timestamp"], 1_760_346_000);
        let state = &update["aps"]["content-state"];
        assert_eq!(state["working"], 2);
        assert_eq!(state["needsYou"], 1);
        assert_eq!(state["done"], 3);
        assert_eq!(state["headline"], "API lane · Shop");
        assert!(update["aps"]["attributes-type"].is_null());

        let start = coding_runs_activity_payload(&data, true);
        assert_eq!(start["aps"]["event"], "start");
        assert_eq!(
            start["aps"]["attributes-type"],
            "CodingRunsActivityAttributes"
        );
        assert_eq!(start["aps"]["attributes"]["title"], "Magician at work");
        assert_eq!(start["aps"]["alert"]["body"], "API lane · Shop");

        let idle = CodingRunsPush {
            active: false,
            start: false,
            ..push
        };
        let end = coding_runs_activity_payload(&idle.data(), false);
        assert_eq!(end["aps"]["event"], "end");
        assert!(end["aps"]["dismissal-date"].is_number());
        // Android gets every field as a string data entry.
        let fields = fcm_data_fields(&idle.data(), false);
        assert_eq!(fields.get("kind").map(String::as_str), Some("coding_runs"));
        assert_eq!(fields.get("active").map(String::as_str), Some("false"));
        assert_eq!(fields.get("needs_you").map(String::as_str), Some("1"));
    }

    #[test]
    fn work_summary_fields_reach_both_phones() {
        let push = CodingRunsPush {
            working: 1,
            needs_you: 1,
            done: 0,
            failed: 0,
            headline: "Ship it · Shop".to_string(),
            detail: "Needs you".to_string(),
            active: true,
            start: false,
            urgent: false,
            event_timestamp: 1_760_346_000_123,
            work: WorkSummaryFields {
                featured_id: "crew:crew_1".to_string(),
                featured_kind: "crew".to_string(),
                featured_state: "needs_you".to_string(),
                featured_title: "Ship it".to_string(),
                featured_context: "Shop".to_string(),
                featured_step: String::new(),
                action_kind: "answer".to_string(),
                action_id: "req_1".to_string(),
                deep_link: "magican://vibe?project=p&crew=crew_1".to_string(),
                more: 2,
                next: vec![WorkSummaryNext {
                    id: "task:t".to_string(),
                    kind: "work_task".to_string(),
                    state: "working".to_string(),
                    title: "Weekly report".to_string(),
                }],
            },
        };
        let data = push.data();
        let update = coding_runs_activity_payload(&data, false);
        let state = &update["aps"]["content-state"];
        assert_eq!(state["featuredKind"], "crew");
        assert_eq!(state["actionId"], "req_1");
        assert_eq!(state["more"], 2);
        assert_eq!(state["next"][0]["title"], "Weekly report");
        assert_eq!(state["deepLink"], "magican://vibe?project=p&crew=crew_1");
        // Android: strings, `next` as its JSON text.
        let fields = fcm_data_fields(&data, false);
        assert_eq!(
            fields.get("featured_id").map(String::as_str),
            Some("crew:crew_1")
        );
        assert!(fields
            .get("next")
            .is_some_and(|next| next.contains("Weekly report")));

        let item = WorkItemPush {
            item_id: "crew:crew_1".to_string(),
            kind: "crew".to_string(),
            state: "done".to_string(),
            title: "Ship it".to_string(),
            context: "Shop".to_string(),
            step: String::new(),
            spent_usd: Some(0.09),
            action_kind: String::new(),
            action_id: String::new(),
            deep_link: "magican://vibe?project=p&crew=crew_1".to_string(),
            ended: true,
            urgent: false,
            event_timestamp: 1_760_346_000_123,
        };
        let end = work_item_activity_payload(&item.data());
        assert_eq!(end["aps"]["event"], "end");
        assert_eq!(end["aps"]["content-state"]["state"], "done");
        assert_eq!(end["aps"]["content-state"]["spentUsd"], 0.09);
        assert!(end["aps"]["dismissal-date"].is_number());
        let fields = fcm_data_fields(&item.data(), false);
        assert_eq!(fields.get("kind").map(String::as_str), Some("work_item"));
        assert_eq!(fields.get("done").map(String::as_str), Some("true"));
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
    fn only_a_plan_approval_offers_approve_and_reject() {
        use crate::magician_v2::hitl_delivery::push_answer_actions;
        assert_eq!(
            push_answer_actions("plan_approval", "confirmation", None),
            Some("approve_reject")
        );
        assert_eq!(
            push_answer_actions(
                "plan_approval",
                "confirmation",
                Some(&json!({"destructive": false}))
            ),
            Some("approve_reject")
        );
        assert_eq!(
            push_answer_actions(
                "plan_approval",
                "confirmation",
                Some(&json!({"destructive": true}))
            ),
            None
        );
        assert_eq!(
            push_answer_actions(
                "plan_approval",
                "confirmation",
                Some(&json!({"sensitive": {"kind": "otp"}}))
            ),
            None
        );
        // Unflagged confirmations from other lanes (agentic action
        // confirmations, secret approvals, max-iterations, budget, approvals)
        // never get one-tap buttons.
        for source in ["agentic", "user_request", "approval", "chat", "budget"] {
            assert_eq!(
                push_answer_actions(source, "confirmation", None),
                None,
                "{source}"
            );
        }
        assert_eq!(push_answer_actions("plan_approval", "choice", None), None);

        let with_actions = application_apns_payload(
            &json!({"kind":"attention_requested","correlation_id":"plan_1","actions":"approve_reject"}),
            true,
        );
        assert_eq!(with_actions["aps"]["category"], ATTENTION_APPROVAL_CATEGORY);
        let without = application_apns_payload(&json!({"kind":"attention_requested"}), true);
        assert!(without["aps"].get("category").is_none());
    }

    #[test]
    fn attention_push_carries_the_answer_mode_and_stays_content_free() {
        use crate::magician_v2::hitl_delivery::{PushAnswer, PushAnswerMode};
        let choice = attention_requested_data(
            "exec-1:p:s~a",
            "agentic",
            PushAnswer {
                actions: None,
                mode: Some(PushAnswerMode::Choice { option_count: 3 }),
            },
            10,
            20,
        );
        assert_eq!(choice["kind"], "attention_requested");
        assert_eq!(choice["source"], "agentic");
        assert_eq!(choice["answer_mode"], "choice");
        assert_eq!(choice["option_count"], 3);
        assert!(choice.get("actions").is_none());
        assert_eq!(choice["deep_link"], attention_deep_link("exec-1:p:s~a"));
        // Only the routing fields and the answer hints: never a prompt,
        // a hint or an option label.
        let mut keys: Vec<&str> = choice
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "answer_mode",
                "correlation_id",
                "deep_link",
                "event_timestamp",
                "expires_at",
                "kind",
                "option_count",
                "source"
            ]
        );

        // APNs: the root keys sit beside `aps`, and the ask's alert runs the
        // Notification Service Extension with the generic copy as fallback.
        let apns = application_apns_payload(&choice, true);
        assert_eq!(apns["aps"]["mutable-content"], 1);
        assert_eq!(apns["aps"]["alert"]["title"], "Magican needs your input");
        assert_eq!(apns["answer_mode"], "choice");
        assert_eq!(apns["option_count"], 3);
        assert!(apns["aps"].get("category").is_none());

        // FCM: the same fields as strings.
        let fcm = fcm_data_fields(&choice, true);
        assert_eq!(fcm["answer_mode"], "choice");
        assert_eq!(fcm["option_count"], "3");
        assert_eq!(fcm["notify"], "true");

        // A plan approval keeps the legacy buttons beside `confirm`.
        let plan = attention_requested_data(
            "plan_1",
            "plan_approval",
            PushAnswer {
                actions: Some("approve_reject"),
                mode: Some(PushAnswerMode::Confirm),
            },
            10,
            20,
        );
        assert_eq!(plan["actions"], "approve_reject");
        assert_eq!(plan["answer_mode"], "confirm");
        assert!(plan.get("option_count").is_none());
        let apns = application_apns_payload(&plan, true);
        assert_eq!(apns["aps"]["category"], ATTENTION_APPROVAL_CATEGORY);
        assert_eq!(apns["aps"]["mutable-content"], 1);
        let fcm = fcm_data_fields(&plan, true);
        assert_eq!(fcm["answer_mode"], "confirm");
        assert!(!fcm.contains_key("option_count"));

        // An ask that must be opened in the app names no mode.
        let open = attention_requested_data("ur-1", "approval", PushAnswer::default(), 10, 20);
        assert!(open.get("answer_mode").is_none());
        assert!(open.get("option_count").is_none());
        assert!(!fcm_data_fields(&open, true).contains_key("answer_mode"));
        assert_eq!(
            application_apns_payload(&open, true)["aps"]["mutable-content"],
            1
        );

        // Silent pushes never ask for the extension.
        for kind in ["attention_resolved", "attention_expired"] {
            let silent = application_apns_payload(&json!({"kind": kind}), false);
            assert!(silent["aps"].get("mutable-content").is_none(), "{kind}");
        }
    }

    #[test]
    fn an_ask_notification_expires_at_its_deadline_or_the_ttl() {
        let now = 1_000_000;
        let hour = Duration::from_secs(3600);
        // No deadline: the TTL.
        assert_eq!(attention_expires_at_ms(now, None, hour), now + 3_600_000);
        // A sooner deadline wins; a later one does not stretch the TTL.
        assert_eq!(
            attention_expires_at_ms(now, Some(now + 600_000), hour),
            now + 600_000
        );
        assert_eq!(
            attention_expires_at_ms(now, Some(now + 7_200_000), hour),
            now + 3_600_000
        );
        // A deadline already at hand still gives the push a minute.
        assert_eq!(
            attention_expires_at_ms(now, Some(now - 5), hour),
            now + 60_000
        );
    }

    #[test]
    fn fcm_ttl_is_whole_seconds_and_never_zero() {
        assert_eq!(fcm_ttl(1_000_000 + 3_600_000, 1_000_000), "3600s");
        assert_eq!(fcm_ttl(1_000_000, 1_000_000), "1s");
    }

    #[test]
    fn each_ask_gets_its_own_short_stable_collapse_id() {
        let one = apns_collapse_id("agent:executive-assistant:run~2413aa35ce07");
        assert_eq!(
            one,
            apns_collapse_id("agent:executive-assistant:run~2413aa35ce07")
        );
        assert_ne!(
            one,
            apns_collapse_id("plan_7cc6c9163f3a4f5bb6ed638b33c2988b")
        );
        assert!(one.len() <= 64);
        assert!(apns_collapse_id(&"x".repeat(500)).len() <= 64);
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
