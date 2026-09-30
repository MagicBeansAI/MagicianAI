//! Phase 7 live custom-surface runtime.
//!
//! Holds no-script host sessions, serves only admitted `surfaces/` bytes,
//! enforces message/byte/time watchdogs, and tears sessions down on
//! disable, quarantine, update or revocation. Scripted surfaces run in
//! a Magician-spawned OS child with CPU/RSS/wall kill. The UI iframe
//! stays no-script and is not that boundary.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    lifecycle::AppInstallationStatus,
    manifest::AppPackageCandidate,
    models::{
        AppContractError, AppContractLimits, AppDigest, AppErrorEnvelope, AppInstallationId,
        AppReference, AppRevision, AppRunStatus,
    },
    records::AppSurfaceStatus,
    registry_lifecycle::AppLifecycleEventKind,
    sandbox::{
        admit_bridge_message, AppBridgeMessage, AppBridgeMethod, AppCustomSurfaceTeardown,
        AppSandboxError, GENERAL_IFRAME_SANDBOX,
    },
    surface_assets::{
        resolve_surface_asset_from_candidate, AppResolvedSurfaceAsset, AppSurfaceAssetAdmission,
        AppSurfaceAssetError, AppSurfaceAssetKind,
    },
    surface_host::{
        compile_no_script_host_from_package, AppNoScriptHostEnvelope, AppSurfaceHostError,
    },
    surface_worker::{
        package_has_javascript, package_has_wasm, spawn_session_worker, AppSurfaceQueuedBridge,
        AppSurfaceWorkerBudget, AppSurfaceWorkerError, AppSurfaceWorkerProcess,
    },
};

const DEFAULT_MAX_MESSAGES: u32 = 32;
const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 256 * 1024;
const DEFAULT_MAX_SESSIONS_PER_INSTALLATION: usize = 8;
const DEFAULT_SESSION_TTL: Duration = Duration::minutes(15);
pub const CUSTOM_SURFACE_MAX_RUN_POLLS: u8 = 16;
pub const CUSTOM_SURFACE_MIN_RUN_POLL_INTERVAL_MS: u64 = 25;
pub const CUSTOM_SURFACE_MAX_RUN_POLL_INTERVAL_MS: u64 = 250;

/// Closed recovery instruction returned only across the private custom-surface
/// worker bridge. It never treats transport abort as canonical cancellation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppCustomSurfaceRetryDisposition {
    None,
    PollRun,
    RetryIdenticalInput,
    OutcomeUncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceRunReadRequest {
    pub run_ref: AppReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceRunWaitRequest {
    pub run_ref: AppReference,
    pub max_polls: u8,
    pub poll_interval_ms: u64,
}

impl AppCustomSurfaceRunWaitRequest {
    pub fn validate(&self) -> Result<(), AppContractError> {
        if self.max_polls == 0 || self.max_polls > CUSTOM_SURFACE_MAX_RUN_POLLS {
            return Err(AppContractError::invalid(
                "max_polls",
                "must be between 1 and 16",
            ));
        }
        if !(CUSTOM_SURFACE_MIN_RUN_POLL_INTERVAL_MS..=CUSTOM_SURFACE_MAX_RUN_POLL_INTERVAL_MS)
            .contains(&self.poll_interval_ms)
        {
            return Err(AppContractError::invalid(
                "poll_interval_ms",
                "must be between 25 and 250 milliseconds",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceRunCancelRequest {
    pub run_ref: AppReference,
    pub expected_generation: u64,
    pub idempotency_key: AppReference,
}

/// Payload-minimal cancellation receipt for a custom-surface worker. The
/// canonical Artifact/workflow receipt remains the owner; this projection
/// strips task/execution coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceCancellationReceipt {
    pub generation: u64,
    pub idempotency_key: AppReference,
    pub status: AppRunStatus,
    pub requested_at: DateTime<Utc>,
}

/// The sole worker-visible action lifecycle record. It deliberately omits
/// task IDs, execution IDs, installation IDs, raw owner errors and receipt
/// identities. Correlation is the opaque logical run plus the request-bound
/// installation/action checked by the authenticated host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppCustomSurfaceRunReply {
    pub run_ref: AppReference,
    pub status: AppRunStatus,
    pub terminal: bool,
    pub result_withheld: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation_generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<AppErrorEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<AppCustomSurfaceCancellationReceipt>,
    pub retry_disposition: AppCustomSurfaceRetryDisposition,
}

impl AppCustomSurfaceRunReply {
    pub fn validate_for(&self, expected_run_ref: &AppReference) -> Result<(), AppContractError> {
        if &self.run_ref != expected_run_ref {
            return Err(AppContractError::invalid(
                "run_ref",
                "must match the requested custom-surface run",
            ));
        }
        if self.terminal != self.status.is_terminal() {
            return Err(AppContractError::invalid(
                "terminal",
                "must match the closed run status",
            ));
        }
        if self.result_withheld && (self.result.is_some() || self.error.is_some()) {
            return Err(AppContractError::invalid(
                "result_withheld",
                "cannot accompany result or error bytes",
            ));
        }
        if self.cancellation_generation == Some(0) {
            return Err(AppContractError::invalid(
                "cancellation_generation",
                "must be positive when present",
            ));
        }
        if let Some(receipt) = self.receipt.as_ref() {
            if receipt.generation == 0
                || self.cancellation_generation != Some(receipt.generation)
                || receipt.status != self.status
            {
                return Err(AppContractError::invalid(
                    "receipt",
                    "must match the canonical cancellation projection",
                ));
            }
        }
        if !self.terminal && (self.result.is_some() || self.error.is_some()) {
            return Err(AppContractError::invalid(
                "result",
                "cannot be returned before terminal settlement",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppCustomSurfaceWatchdog {
    pub max_messages: u32,
    pub max_payload_bytes: u64,
    pub max_sessions_per_installation: usize,
    pub session_ttl: Duration,
    pub killable_worker_available: bool,
}

impl Default for AppCustomSurfaceWatchdog {
    fn default() -> Self {
        Self {
            max_messages: DEFAULT_MAX_MESSAGES,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_sessions_per_installation: DEFAULT_MAX_SESSIONS_PER_INSTALLATION,
            session_ttl: DEFAULT_SESSION_TTL,
            killable_worker_available: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AppCustomSurfaceHostResponse {
    pub srcdoc: String,
    pub sandbox: String,
    pub csp: String,
    pub allowed_assets: Vec<String>,
    pub session_ref: String,
    pub nonce: String,
    pub installation_id: String,
    pub package_revision_ref: String,
    pub surface_revision: u64,
    pub grant_revision: u64,
    /// Canonical durable entity-change head filled by the authenticated host
    /// owner before the response crosses HTTP.
    pub change_sequence: u64,
    pub envelope_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_render: Option<String>,
}

impl From<&AppNoScriptHostEnvelope> for AppCustomSurfaceHostResponse {
    fn from(envelope: &AppNoScriptHostEnvelope) -> Self {
        Self {
            srcdoc: envelope.srcdoc.clone(),
            sandbox: envelope.sandbox.clone(),
            csp: envelope.csp.clone(),
            allowed_assets: envelope
                .allowed_assets
                .iter()
                .map(|path| path.as_str().to_owned())
                .collect(),
            session_ref: envelope.session.session_ref.as_str().to_owned(),
            nonce: envelope.session.nonce.as_str().to_owned(),
            installation_id: envelope.session.installation_id.as_str().to_owned(),
            package_revision_ref: envelope.session.package_revision_ref.as_str().to_owned(),
            surface_revision: envelope.session.surface_revision.get(),
            grant_revision: envelope.session.grant_revision.get(),
            change_sequence: 0,
            envelope_digest: envelope.envelope_digest.as_str().to_owned(),
            worker_pid: None,
            worker_entry: None,
            last_render: None,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppCustomSurfaceRuntimeError {
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Host(#[from] AppSurfaceHostError),
    #[error(transparent)]
    Asset(#[from] AppSurfaceAssetError),
    #[error(transparent)]
    Bridge(#[from] AppSandboxError),
    #[error("custom-surface session is unknown or torn down")]
    SessionGone,
    #[error("custom-surface watchdog refused the request")]
    WatchdogTripped,
    #[error("scripted custom surfaces require a killable worker")]
    KillableWorkerRequired,
    #[error("custom-surface session limit for this installation was reached")]
    SessionLimit,
    #[error("custom-surface wasm is refused until a wasm worker exists")]
    WasmRefused,
    #[error(transparent)]
    Worker(#[from] AppSurfaceWorkerError),
}

struct LiveSession {
    envelope: AppNoScriptHostEnvelope,
    message_count: u32,
    payload_bytes: u64,
    opened_at: DateTime<Utc>,
    torn_down: Option<AppCustomSurfaceTeardown>,
    worker: Option<AppSurfaceWorkerProcess>,
}

pub struct AppCustomSurfaceRuntime {
    watchdog: AppCustomSurfaceWatchdog,
    sessions: Arc<Mutex<HashMap<AppReference, LiveSession>>>,
    stop_budget: Arc<AtomicBool>,
    budget_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Default for AppCustomSurfaceRuntime {
    fn default() -> Self {
        Self::new(AppCustomSurfaceWatchdog::default())
    }
}

impl AppCustomSurfaceRuntime {
    pub fn new(watchdog: AppCustomSurfaceWatchdog) -> Self {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let stop_budget = Arc::new(AtomicBool::new(false));
        let budget_thread = if watchdog.killable_worker_available {
            Some(spawn_session_budget_thread(
                Arc::clone(&sessions),
                Arc::clone(&stop_budget),
            ))
        } else {
            None
        };
        Self {
            watchdog,
            sessions,
            stop_budget,
            budget_thread: Mutex::new(budget_thread),
        }
    }

    pub fn with_killable_worker() -> Self {
        Self::new(AppCustomSurfaceWatchdog {
            killable_worker_available: true,
            ..AppCustomSurfaceWatchdog::default()
        })
    }

    pub fn open_no_script_host(
        &self,
        candidate: &AppPackageCandidate,
        document_path: &str,
        admission: AppSurfaceAssetAdmission<'_>,
        now: DateTime<Utc>,
    ) -> Result<AppNoScriptHostEnvelope, AppCustomSurfaceRuntimeError> {
        let envelope = compile_no_script_host_from_package(candidate, document_path, admission)?;
        if envelope.sandbox == GENERAL_IFRAME_SANDBOX
            || envelope.sandbox.contains("allow-scripts")
            || envelope.sandbox.contains("allow-same-origin")
        {
            return Err(AppSurfaceHostError::GeneralIframeTokensRefused.into());
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let open_for_install = sessions
            .values()
            .filter(|live| {
                live.torn_down.is_none()
                    && live.envelope.session.installation_id == envelope.session.installation_id
            })
            .count();
        if open_for_install >= self.watchdog.max_sessions_per_installation {
            return Err(AppCustomSurfaceRuntimeError::SessionLimit);
        }
        sessions.insert(
            envelope.session.session_ref.clone(),
            LiveSession {
                envelope: envelope.clone(),
                message_count: 0,
                payload_bytes: 0,
                opened_at: now,
                torn_down: None,
                worker: None,
            },
        );
        Ok(envelope)
    }

    pub fn open_scripted_host(
        &self,
        candidate: &AppPackageCandidate,
        document_path: &str,
        admission: AppSurfaceAssetAdmission<'_>,
        now: DateTime<Utc>,
    ) -> Result<AppNoScriptHostEnvelope, AppCustomSurfaceRuntimeError> {
        self.require_killable_worker_for_scripts()?;
        if package_has_wasm(candidate) {
            return Err(AppCustomSurfaceRuntimeError::WasmRefused);
        }
        if !package_has_javascript(candidate) {
            return self.open_no_script_host(candidate, document_path, admission, now);
        }
        let envelope = self.open_no_script_host(candidate, document_path, admission, now)?;
        let mut worker = match spawn_session_worker(
            candidate,
            envelope.session.session_ref.as_str(),
            envelope.session.installation_id.as_str(),
            envelope.session.package_revision_ref.as_str(),
            AppSurfaceWorkerBudget::default(),
        ) {
            Ok(worker) => worker,
            Err(error) => {
                self.teardown_session(
                    &envelope.session.session_ref,
                    AppCustomSurfaceTeardown::Disable,
                );
                return Err(error.into());
            },
        };
        if let Err(error) = worker.wait_ready(StdDuration::from_secs(8)) {
            worker.kill();
            self.teardown_session(
                &envelope.session.session_ref,
                AppCustomSurfaceTeardown::Disable,
            );
            return Err(error.into());
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(live) = sessions.get_mut(&envelope.session.session_ref) {
            live.worker = Some(worker);
        } else {
            worker.kill();
            return Err(AppCustomSurfaceRuntimeError::SessionGone);
        }
        Ok(envelope)
    }

    /// Drain only worker-originated bridge requests for one live,
    /// revision-bound session. This owns no data-plane execution; the
    /// authenticated API owner admits and executes each returned request
    /// before calling [`Self::complete_worker_bridge`].
    pub fn take_worker_bridge_requests(
        &self,
        session_ref: &AppReference,
    ) -> Result<Vec<AppSurfaceQueuedBridge>, AppCustomSurfaceRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions
            .get_mut(session_ref)
            .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
        if live.torn_down.is_some() {
            return Err(AppCustomSurfaceRuntimeError::SessionGone);
        }
        let worker = live
            .worker
            .as_mut()
            .ok_or(AppCustomSurfaceRuntimeError::KillableWorkerRequired)?;
        worker.poll_budget()?;
        let requests = worker.take_bridge_requests();
        if worker.last_error.is_some() {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppSurfaceWorkerError::Protocol.into());
        }
        if !worker.is_alive() && worker.last_render.is_none() {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppSurfaceWorkerError::Protocol.into());
        }
        Ok(requests)
    }

    /// Derive a canonical bridge message from the live session. Worker bytes
    /// cannot select installation, package, surface, grant, nonce, origin, or
    /// host-session authority.
    pub fn admit_worker_bridge(
        &self,
        session_ref: &AppReference,
        request: &AppSurfaceQueuedBridge,
        now: DateTime<Utc>,
        expected_host_session_ref: &AppReference,
        live_package_revision_ref: &AppReference,
        live_surface_revision: AppRevision,
        live_grant_revision: AppRevision,
        limits: &AppContractLimits,
    ) -> Result<AppBridgeMessage, AppCustomSurfaceRuntimeError> {
        let (method, view_or_action) = match request.method.as_str() {
            "query" => (
                AppBridgeMethod::Query,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            "mutate" => (
                AppBridgeMethod::Mutate,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            "invoke" => (
                AppBridgeMethod::InvokeAction,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            "subscribe" if request.view_or_action == "changes" => {
                (AppBridgeMethod::Subscribe, None)
            },
            "get_run" => (
                AppBridgeMethod::GetActionRun,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            "wait_run" => (
                AppBridgeMethod::WaitActionRun,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            "cancel_run" => (
                AppBridgeMethod::CancelActionRun,
                Some(super::models::AppName::parse(
                    request.view_or_action.clone(),
                )?),
            ),
            _ => return Err(AppSandboxError::UnknownMethod.into()),
        };
        let request_id = AppReference::parse(format!("worker-request:{}", request.sequence))?;
        if request.request_id != request_id.as_str() {
            return Err(AppSandboxError::SessionMismatch.into());
        }
        let (installation_id, package_revision_ref, surface_revision, grant_revision, nonce) = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let live = sessions
                .get(session_ref)
                .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
            if live.torn_down.is_some() || live.worker.is_none() {
                return Err(AppCustomSurfaceRuntimeError::SessionGone);
            }
            if live.envelope.session.host_session_ref != *expected_host_session_ref {
                return Err(AppSandboxError::SessionMismatch.into());
            }
            (
                live.envelope.session.installation_id.clone(),
                live.envelope.session.package_revision_ref.clone(),
                live.envelope.session.surface_revision,
                live.envelope.session.grant_revision,
                live.envelope.session.nonce.clone(),
            )
        };
        let message = AppBridgeMessage {
            schema_version: 1,
            request_id,
            sequence: request.sequence,
            method,
            origin: "null".to_owned(),
            session_ref: session_ref.clone(),
            nonce,
            installation_id,
            package_revision_ref,
            surface_revision,
            grant_revision,
            view_or_action,
            payload: request.payload.clone(),
        };
        self.admit_bridge_for_host(
            &message,
            now,
            live_package_revision_ref,
            live_surface_revision,
            live_grant_revision,
            expected_host_session_ref,
            "null",
            limits,
        )?;
        Ok(message)
    }

    pub fn complete_worker_bridge(
        &self,
        session_ref: &AppReference,
        request: &AppSurfaceQueuedBridge,
        result: Result<serde_json::Value, (&str, &str)>,
    ) -> Result<(), AppCustomSurfaceRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions
            .get_mut(session_ref)
            .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
        if live.torn_down.is_some() {
            return Err(AppCustomSurfaceRuntimeError::SessionGone);
        }
        let worker = live
            .worker
            .as_mut()
            .ok_or(AppCustomSurfaceRuntimeError::KillableWorkerRequired)?;
        worker.complete_bridge(&request.request_id, request.sequence, result)?;
        Ok(())
    }

    pub fn worker_render_ready(
        &self,
        session_ref: &AppReference,
    ) -> Result<bool, AppCustomSurfaceRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions
            .get_mut(session_ref)
            .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
        if live.torn_down.is_some() {
            return Err(AppCustomSurfaceRuntimeError::SessionGone);
        }
        let worker = live
            .worker
            .as_mut()
            .ok_or(AppCustomSurfaceRuntimeError::KillableWorkerRequired)?;
        worker.drain_events();
        if worker.last_error.is_some() {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppSurfaceWorkerError::Protocol.into());
        }
        if worker.last_render.is_none() && !worker.is_alive() {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppSurfaceWorkerError::Protocol.into());
        }
        Ok(worker.last_render.is_some())
    }

    pub fn abort_worker_pump(&self, session_ref: &AppReference) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(mut live) = sessions.remove(session_ref) {
            if let Some(worker) = live.worker.as_mut() {
                worker.kill();
            }
        }
    }

    /// Retire the previous host session only after its replacement has opened
    /// successfully. A stale/unknown reference is harmless reconnect state;
    /// a reference owned by another installation is never touched.
    pub fn retire_replaced_session(
        &self,
        installation_id: &AppInstallationId,
        host_session_ref: &AppReference,
        previous_session_ref: &AppReference,
        current_session_ref: &AppReference,
    ) {
        if previous_session_ref == current_session_ref {
            return;
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(previous) = sessions.get(previous_session_ref) else {
            return;
        };
        if previous.envelope.session.installation_id != *installation_id
            || previous.envelope.session.host_session_ref != *host_session_ref
        {
            return;
        }
        let Some(mut previous) = sessions.remove(previous_session_ref) else {
            return;
        };
        if let Some(worker) = previous.worker.as_mut() {
            worker.kill();
        }
    }

    pub fn serve_asset(
        &self,
        candidate: &AppPackageCandidate,
        session_ref: &AppReference,
        path: &str,
        admission: AppSurfaceAssetAdmission<'_>,
    ) -> Result<AppResolvedSurfaceAsset, AppCustomSurfaceRuntimeError> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions
            .get(session_ref)
            .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
        if live.torn_down.is_some() {
            return Err(AppCustomSurfaceRuntimeError::SessionGone);
        }
        if !live
            .envelope
            .allowed_assets
            .iter()
            .any(|allowed| allowed.as_str() == path)
        {
            return Err(AppSurfaceAssetError::MissingMember.into());
        }
        let asset = resolve_surface_asset_from_candidate(
            candidate.members(),
            path,
            admission,
            super::sandbox::AppCustomSurfaceMode::DeclarativeNoScript,
            self.watchdog.killable_worker_available,
        )?;
        if asset.kind != AppSurfaceAssetKind::Asset {
            return Err(AppCustomSurfaceRuntimeError::from(
                AppSurfaceHostError::InvalidAdmittedAsset,
            ));
        }
        Ok(asset)
    }

    pub fn admit_bridge(
        &self,
        message: &AppBridgeMessage,
        now: DateTime<Utc>,
        live_package_revision_ref: &AppReference,
        live_surface_revision: AppRevision,
        live_grant_revision: AppRevision,
        expected_origin: &str,
        limits: &AppContractLimits,
    ) -> Result<(), AppCustomSurfaceRuntimeError> {
        let host_session_ref = {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            sessions
                .get(&message.session_ref)
                .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?
                .envelope
                .session
                .host_session_ref
                .clone()
        };
        self.admit_bridge_for_host(
            message,
            now,
            live_package_revision_ref,
            live_surface_revision,
            live_grant_revision,
            &host_session_ref,
            expected_origin,
            limits,
        )
    }

    pub fn admit_bridge_for_host(
        &self,
        message: &AppBridgeMessage,
        now: DateTime<Utc>,
        live_package_revision_ref: &AppReference,
        live_surface_revision: AppRevision,
        live_grant_revision: AppRevision,
        expected_host_session_ref: &AppReference,
        expected_origin: &str,
        limits: &AppContractLimits,
    ) -> Result<(), AppCustomSurfaceRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let live = sessions
            .get_mut(&message.session_ref)
            .ok_or(AppCustomSurfaceRuntimeError::SessionGone)?;
        if live.torn_down.is_some() {
            return Err(AppSandboxError::SessionTornDown.into());
        }
        if live.envelope.session.host_session_ref != *expected_host_session_ref {
            return Err(AppSandboxError::SessionMismatch.into());
        }
        if let Some(worker) = live.worker.as_mut() {
            worker.drain_events();
            if let Err(error) = worker.poll_budget() {
                live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
                return Err(error.into());
            }
        }
        if now >= live.opened_at + self.watchdog.session_ttl {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppCustomSurfaceRuntimeError::WatchdogTripped);
        }
        let payload_len = serde_json::to_vec(&message.payload)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(u64::MAX);
        if live.message_count >= self.watchdog.max_messages
            || live.payload_bytes.saturating_add(payload_len) > self.watchdog.max_payload_bytes
        {
            live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
            return Err(AppCustomSurfaceRuntimeError::WatchdogTripped);
        }
        admit_bridge_message(
            &mut live.envelope.session,
            message,
            now,
            live_package_revision_ref,
            live_surface_revision,
            live_grant_revision,
            expected_origin,
            limits,
        )?;
        live.message_count = live.message_count.saturating_add(1);
        live.payload_bytes = live.payload_bytes.saturating_add(payload_len);
        Ok(())
    }

    pub fn teardown_installation(
        &self,
        installation_id: &AppInstallationId,
        reason: AppCustomSurfaceTeardown,
    ) -> usize {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut torn = 0usize;
        for live in sessions.values_mut() {
            if live.envelope.session.installation_id == *installation_id && live.torn_down.is_none()
            {
                if let Some(worker) = live.worker.as_mut() {
                    worker.kill();
                }
                live.torn_down = Some(reason);
                torn = torn.saturating_add(1);
            }
        }
        torn
    }

    pub fn teardown_for_lifecycle_event(
        &self,
        installation_id: &AppInstallationId,
        kind: AppLifecycleEventKind,
    ) -> usize {
        let Some(reason) = teardown_reason_for_lifecycle(kind) else {
            return 0;
        };
        self.teardown_installation(installation_id, reason)
    }

    pub fn require_killable_worker_for_scripts(&self) -> Result<(), AppCustomSurfaceRuntimeError> {
        if self.watchdog.killable_worker_available {
            Ok(())
        } else {
            Err(AppCustomSurfaceRuntimeError::KillableWorkerRequired)
        }
    }

    pub fn host_response(envelope: &AppNoScriptHostEnvelope) -> AppCustomSurfaceHostResponse {
        AppCustomSurfaceHostResponse::from(envelope)
    }

    /// Reuse the canonical app-value preflight for an owner result before it
    /// crosses the private worker socket.
    pub fn decode_worker_bridge_result(
        bytes: &[u8],
        limits: &AppContractLimits,
    ) -> Result<serde_json::Value, AppCustomSurfaceRuntimeError> {
        Ok(super::models::decode_bounded_json_value(bytes, limits)?)
    }

    /// Re-parse every action/run reply through the closed worker DTO before it
    /// is written to the private socket. This is the final response-
    /// substitution fence and mechanically rejects task/execution IDs or any
    /// other unknown owner field.
    pub fn validate_worker_bridge_result(
        message: &AppBridgeMessage,
        value: &Value,
    ) -> Result<(), AppCustomSurfaceRuntimeError> {
        let expected_run_ref = match message.method {
            AppBridgeMethod::InvokeAction => None,
            AppBridgeMethod::GetActionRun => Some(
                serde_json::from_value::<AppCustomSurfaceRunReadRequest>(message.payload.clone())
                    .map_err(|error| AppContractError::InvalidJson {
                        message: error.to_string(),
                    })?
                    .run_ref,
            ),
            AppBridgeMethod::WaitActionRun => Some(
                serde_json::from_value::<AppCustomSurfaceRunWaitRequest>(message.payload.clone())
                    .map_err(|error| AppContractError::InvalidJson {
                        message: error.to_string(),
                    })?
                    .run_ref,
            ),
            AppBridgeMethod::CancelActionRun => Some(
                serde_json::from_value::<AppCustomSurfaceRunCancelRequest>(message.payload.clone())
                    .map_err(|error| AppContractError::InvalidJson {
                        message: error.to_string(),
                    })?
                    .run_ref,
            ),
            AppBridgeMethod::Query | AppBridgeMethod::Mutate | AppBridgeMethod::Subscribe => {
                return Ok(())
            },
        };
        let reply: AppCustomSurfaceRunReply =
            serde_json::from_value(value.clone()).map_err(|error| {
                AppContractError::InvalidJson {
                    message: error.to_string(),
                }
            })?;
        let expected_run_ref = expected_run_ref.as_ref().unwrap_or(&reply.run_ref);
        reply.validate_for(expected_run_ref)?;
        Ok(())
    }

    pub fn decorate_host_response(&self, response: &mut AppCustomSurfaceHostResponse) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Ok(session_ref) = AppReference::parse(response.session_ref.clone()) else {
            return;
        };
        let Some(live) = sessions.get_mut(&session_ref) else {
            return;
        };
        if let Some(worker) = live.worker.as_mut() {
            worker.drain_events();
            response.worker_pid = Some(worker.pid);
            response.worker_entry = worker.entry.clone();
            response.last_render = worker.last_render.clone();
        }
    }

    pub fn content_security_policy(envelope: &AppNoScriptHostEnvelope) -> &str {
        envelope.csp.as_str()
    }

    fn teardown_session(&self, session_ref: &AppReference, reason: AppCustomSurfaceTeardown) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(live) = sessions.get_mut(session_ref) {
            if let Some(worker) = live.worker.as_mut() {
                worker.kill();
            }
            live.torn_down = Some(reason);
        }
    }
}

impl Drop for AppCustomSurfaceRuntime {
    fn drop(&mut self) {
        self.stop_budget.store(true, Ordering::SeqCst);
        if let Some(handle) = self
            .budget_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = handle.join();
        }
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for live in sessions.values_mut() {
            if let Some(worker) = live.worker.as_mut() {
                worker.kill();
            }
        }
    }
}

fn spawn_session_budget_thread(
    sessions: Arc<Mutex<HashMap<AppReference, LiveSession>>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(StdDuration::from_millis(200));
            let mut sessions = sessions.lock().unwrap_or_else(|error| error.into_inner());
            for live in sessions.values_mut() {
                if live.torn_down.is_some() {
                    continue;
                }
                if let Some(worker) = live.worker.as_mut() {
                    if worker.poll_budget().is_err() {
                        live.torn_down = Some(AppCustomSurfaceTeardown::Disable);
                    }
                }
            }
        }
    })
}

pub fn teardown_reason_for_lifecycle(
    kind: AppLifecycleEventKind,
) -> Option<AppCustomSurfaceTeardown> {
    match kind {
        AppLifecycleEventKind::InstallationDisabled
        | AppLifecycleEventKind::InstallationRetained => Some(AppCustomSurfaceTeardown::Disable),
        AppLifecycleEventKind::InstallationQuarantined => {
            Some(AppCustomSurfaceTeardown::Quarantine)
        },
        AppLifecycleEventKind::InstallationUpdated
        | AppLifecycleEventKind::InstallationReinstalled
        | AppLifecycleEventKind::InstallationRolledBack
        | AppLifecycleEventKind::UpdateBegan => Some(AppCustomSurfaceTeardown::Update),
        AppLifecycleEventKind::GrantRevoked => Some(AppCustomSurfaceTeardown::Revocation),
        AppLifecycleEventKind::InstallationEnabled
        | AppLifecycleEventKind::InstallationReenabled
        | AppLifecycleEventKind::UpdateFailed => None,
    }
}

pub fn surface_admission_for_enabled_installation<'a>(
    package_revision_ref: &'a AppReference,
    bundle_digest: &'a AppDigest,
    installation_id: AppInstallationId,
    surface_revision: AppRevision,
    grant_revision: AppRevision,
    host_session_ref: AppReference,
    session_ref: AppReference,
    nonce: AppReference,
    now: DateTime<Utc>,
) -> AppSurfaceAssetAdmission<'a> {
    AppSurfaceAssetAdmission {
        package_revision_ref,
        live_package_revision_ref: package_revision_ref,
        bundle_digest,
        live_bundle_digest: bundle_digest,
        installation_id,
        installation_status: AppInstallationStatus::Enabled,
        surface_status: AppSurfaceStatus::Active,
        surface_revision,
        grant_revision,
        host_session_ref,
        session_ref,
        nonce,
        now,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use crate::apps::sandbox::{AppBridgeMethod, AppCustomSurfaceMode, GENERAL_IFRAME_SANDBOX};
    use magician::magician_v2::apps::manifest::{
        build_app_package_candidate, tests::valid_skill_document, AppBundleMember, AppPackageLimits,
    };

    fn time(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 18, 21, 0, second)
            .single()
            .unwrap()
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    fn candidate() -> AppPackageCandidate {
        build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/index.html",
                    b"<html><body>plan</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/theme.css", b"body{color:navy}".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("surfaces/app.js", b"alert(1)".to_vec()).unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate")
    }

    fn admission<'a>(
        package: &'a AppReference,
        bundle: &'a AppDigest,
        session: AppReference,
    ) -> AppSurfaceAssetAdmission<'a> {
        surface_admission_for_enabled_installation(
            package,
            bundle,
            AppInstallationId::parse("install_1").unwrap(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            reference("host:session-1"),
            session,
            reference("nonce:1"),
            time(1),
        )
    }

    fn message(session: &AppReference, request: &str) -> AppBridgeMessage {
        AppBridgeMessage {
            schema_version: 1,
            request_id: reference(request),
            sequence: 1,
            method: AppBridgeMethod::Query,
            origin: "null".to_owned(),
            session_ref: session.clone(),
            nonce: reference("nonce:1"),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            package_revision_ref: reference("package-revision:reading-list"),
            surface_revision: AppRevision::new(1).unwrap(),
            grant_revision: AppRevision::new(1).unwrap(),
            view_or_action: Some(
                magician::magician_v2::apps::models::AppName::parse("items").unwrap(),
            ),
            payload: serde_json::json!({"select": ["title"]}),
        }
    }

    #[test]
    fn live_host_serves_srcdoc_and_only_admitted_surfaces_assets() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let envelope = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .expect("open");
        let response = AppCustomSurfaceRuntime::host_response(&envelope);
        assert!(response.srcdoc.contains("<style>body{color:navy}</style>"));
        assert!(response.srcdoc.contains("<body>plan</body>"));
        assert!(response.sandbox.is_empty());
        assert_ne!(response.sandbox, GENERAL_IFRAME_SANDBOX);
        assert!(response.csp.contains("default-src 'none'"));
        assert!(response.csp.contains("style-src 'unsafe-inline'"));
        assert_eq!(response.allowed_assets, vec!["surfaces/theme.css"]);
        assert_eq!(response.session_ref, envelope.session.session_ref.as_str());
        assert_eq!(response.nonce, "nonce:1");
        assert_eq!(response.installation_id, "install_1");
        let css = runtime
            .serve_asset(
                &package,
                &envelope.session.session_ref,
                "surfaces/theme.css",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
            )
            .expect("css");
        assert_eq!(css.media_type(), "text/css; charset=utf-8");
        let escaped = runtime.serve_asset(
            &package,
            &envelope.session.session_ref,
            "SKILL.md",
            admission(&package_ref, &bundle, reference("bridge:session-1")),
        );
        assert!(escaped.is_err());
        let js = runtime.serve_asset(
            &package,
            &envelope.session.session_ref,
            "surfaces/app.js",
            admission(&package_ref, &bundle, reference("bridge:session-1")),
        );
        assert!(js.is_err());
    }

    #[test]
    fn lifecycle_events_tear_sessions_down() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let envelope = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .unwrap();
        assert_eq!(
            runtime.teardown_for_lifecycle_event(
                &envelope.session.installation_id,
                AppLifecycleEventKind::InstallationDisabled,
            ),
            1
        );
        let error = runtime.admit_bridge(
            &message(&envelope.session.session_ref, "req:after-disable"),
            time(2),
            &package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            "null",
            &AppContractLimits::default(),
        );
        assert!(error.is_err());
        for kind in [
            AppLifecycleEventKind::InstallationQuarantined,
            AppLifecycleEventKind::InstallationUpdated,
            AppLifecycleEventKind::GrantRevoked,
        ] {
            let session = reference(&format!("bridge:{kind:?}"));
            runtime
                .open_no_script_host(
                    &package,
                    "surfaces/index.html",
                    admission(&package_ref, &bundle, session.clone()),
                    time(3),
                )
                .unwrap();
            assert_eq!(
                runtime.teardown_for_lifecycle_event(
                    &AppInstallationId::parse("install_1").unwrap(),
                    kind,
                ),
                1
            );
        }
    }

    #[test]
    fn replacement_and_abort_remove_only_the_owned_session() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let previous = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:previous")),
                time(1),
            )
            .expect("previous host");
        let current = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:current")),
                time(1),
            )
            .expect("current host");
        runtime.retire_replaced_session(
            &AppInstallationId::parse("install_other").unwrap(),
            &reference("host:session-1"),
            &previous.session.session_ref,
            &current.session.session_ref,
        );
        runtime.retire_replaced_session(
            &AppInstallationId::parse("install_1").unwrap(),
            &reference("host:another-session"),
            &previous.session.session_ref,
            &current.session.session_ref,
        );
        assert!(matches!(
            runtime.admit_bridge_for_host(
                &message(&previous.session.session_ref, "req:wrong-host"),
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &reference("host:another-session"),
                "null",
                &AppContractLimits::default(),
            ),
            Err(AppCustomSurfaceRuntimeError::Bridge(
                AppSandboxError::SessionMismatch
            ))
        ));
        runtime
            .admit_bridge(
                &message(&previous.session.session_ref, "req:previous-still-live"),
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .expect("cross-installation replacement cannot retire the session");
        runtime.retire_replaced_session(
            &AppInstallationId::parse("install_1").unwrap(),
            &reference("host:session-1"),
            &previous.session.session_ref,
            &current.session.session_ref,
        );
        assert!(runtime
            .admit_bridge(
                &message(&previous.session.session_ref, "req:previous-retired"),
                time(3),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .is_err());
        runtime.abort_worker_pump(&current.session.session_ref);
        assert!(runtime
            .admit_bridge(
                &message(&current.session.session_ref, "req:current-aborted"),
                time(3),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .is_err());
    }

    #[test]
    fn watchdog_trips_on_message_flood_and_scripts_need_a_killable_worker() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog {
            max_messages: 1,
            ..AppCustomSurfaceWatchdog::default()
        });
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let envelope = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .unwrap();
        runtime
            .admit_bridge(
                &message(&envelope.session.session_ref, "req:1"),
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .expect("first");
        let flood = runtime.admit_bridge(
            &message(&envelope.session.session_ref, "req:2"),
            time(3),
            &package_ref,
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
            "null",
            &AppContractLimits::default(),
        );
        assert_eq!(flood, Err(AppCustomSurfaceRuntimeError::WatchdogTripped));
        assert_eq!(
            runtime.require_killable_worker_for_scripts(),
            Err(AppCustomSurfaceRuntimeError::KillableWorkerRequired)
        );
        let _ = AppCustomSurfaceMode::DeclarativeNoScript;
    }

    #[test]
    fn qualification_refuses_forged_replay_stale_cross_app_and_ttl() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog::default());
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let envelope = runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .unwrap();
        let limits = AppContractLimits::default();
        let first = message(&envelope.session.session_ref, "req:1");
        runtime
            .admit_bridge(
                &first,
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            )
            .expect("first");
        assert!(matches!(
            runtime.admit_bridge(
                &first,
                time(3),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            ),
            Err(AppCustomSurfaceRuntimeError::Bridge(
                AppSandboxError::Replay
            ))
        ));

        let mut forged = message(&envelope.session.session_ref, "req:forged");
        forged.origin = "https://evil.example".to_owned();
        assert!(matches!(
            runtime.admit_bridge(
                &forged,
                time(4),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            ),
            Err(AppCustomSurfaceRuntimeError::Bridge(
                AppSandboxError::ForgedOrigin
            ))
        ));

        let mut stale = message(&envelope.session.session_ref, "req:stale");
        stale.package_revision_ref = reference("package-revision:other");
        assert!(matches!(
            runtime.admit_bridge(
                &stale,
                time(5),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            ),
            Err(AppCustomSurfaceRuntimeError::Bridge(
                AppSandboxError::StaleRevision
            ))
        ));

        let mut cross_app = message(&envelope.session.session_ref, "req:cross-app");
        cross_app.installation_id = AppInstallationId::parse("install_2").unwrap();
        assert!(matches!(
            runtime.admit_bridge(
                &cross_app,
                time(6),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            ),
            Err(AppCustomSurfaceRuntimeError::Bridge(
                AppSandboxError::SessionMismatch
            ))
        ));

        assert_eq!(
            runtime.admit_bridge(
                &message(&envelope.session.session_ref, "req:ttl"),
                time(1) + Duration::minutes(16),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &limits,
            ),
            Err(AppCustomSurfaceRuntimeError::WatchdogTripped)
        );
    }

    #[test]
    fn qualification_session_limit_byte_flood_and_lifecycle_map() {
        let runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog {
            max_sessions_per_installation: 1,
            max_payload_bytes: 24,
            ..AppCustomSurfaceWatchdog::default()
        });
        let package = candidate();
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .unwrap();
        assert_eq!(
            runtime.open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-2")),
                time(2),
            ),
            Err(AppCustomSurfaceRuntimeError::SessionLimit)
        );

        let flood_runtime = AppCustomSurfaceRuntime::new(AppCustomSurfaceWatchdog {
            max_payload_bytes: 24,
            ..AppCustomSurfaceWatchdog::default()
        });
        let envelope = flood_runtime
            .open_no_script_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-1")),
                time(1),
            )
            .unwrap();
        flood_runtime
            .admit_bridge(
                &message(&envelope.session.session_ref, "req:1"),
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .expect("first small payload");
        assert_eq!(
            flood_runtime.admit_bridge(
                &message(&envelope.session.session_ref, "req:2"),
                time(3),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            ),
            Err(AppCustomSurfaceRuntimeError::WatchdogTripped)
        );

        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::InstallationDisabled),
            Some(AppCustomSurfaceTeardown::Disable)
        );
        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::InstallationQuarantined),
            Some(AppCustomSurfaceTeardown::Quarantine)
        );
        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::InstallationUpdated),
            Some(AppCustomSurfaceTeardown::Update)
        );
        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::GrantRevoked),
            Some(AppCustomSurfaceTeardown::Revocation)
        );
        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::InstallationEnabled),
            None
        );
        assert_eq!(
            teardown_reason_for_lifecycle(AppLifecycleEventKind::UpdateFailed),
            None
        );
    }

    #[test]
    fn scripted_host_spawns_a_killable_worker_and_lifecycle_kills_it() {
        let runtime = AppCustomSurfaceRuntime::with_killable_worker();
        let package = build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/index.html",
                    b"<html><body>plan</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/app.js",
                    b"const value = magician.query('items', { select: ['title'] }); magician.render(value.title);".to_vec(),
                )
                .unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate");
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        let envelope = runtime
            .open_scripted_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-js")),
                time(1),
            )
            .expect("scripted open");
        let request = (0..100)
            .find_map(|_| {
                let mut requests = runtime
                    .take_worker_bridge_requests(&envelope.session.session_ref)
                    .expect("drain worker requests");
                let request = requests.pop();
                if request.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                request
            })
            .expect("worker query");
        let admitted = runtime
            .admit_worker_bridge(
                &envelope.session.session_ref,
                &request,
                time(2),
                &reference("host:session-1"),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                &AppContractLimits::default(),
            )
            .expect("worker query admitted");
        assert_eq!(admitted.method, AppBridgeMethod::Query);
        assert_eq!(admitted.installation_id.as_str(), "install_1");
        runtime
            .complete_worker_bridge(
                &envelope.session.session_ref,
                &request,
                Ok(serde_json::json!({"title": "from-worker"})),
            )
            .expect("worker query response");
        assert!((0..100).any(|_| {
            let ready = runtime
                .worker_render_ready(&envelope.session.session_ref)
                .expect("poll worker render");
            if !ready {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            ready
        }));
        let mut response = AppCustomSurfaceRuntime::host_response(&envelope);
        runtime.decorate_host_response(&mut response);
        assert!(response.worker_pid.is_some());
        assert_eq!(response.worker_entry.as_deref(), Some("surfaces/app.js"));
        assert_eq!(response.last_render.as_deref(), Some("from-worker"));
        assert!(response.sandbox.is_empty());
        let pid = response.worker_pid.expect("pid");
        assert_eq!(
            runtime.teardown_for_lifecycle_event(
                &envelope.session.installation_id,
                AppLifecycleEventKind::GrantRevoked,
            ),
            1
        );
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(sysinfo_process_gone(pid));
        assert!(runtime
            .admit_bridge(
                &message(&envelope.session.session_ref, "req:after-kill"),
                time(2),
                &package_ref,
                AppRevision::new(1).unwrap(),
                AppRevision::new(1).unwrap(),
                "null",
                &AppContractLimits::default(),
            )
            .is_err());
    }

    #[test]
    fn wasm_package_is_refused_even_with_a_worker() {
        let runtime = AppCustomSurfaceRuntime::with_killable_worker();
        let package = build_app_package_candidate(
            vec![
                AppBundleMember::regular_file("SKILL.md", valid_skill_document().into_bytes())
                    .unwrap(),
                AppBundleMember::regular_file("workflows/build.md", b"Build a plan.".to_vec())
                    .unwrap(),
                AppBundleMember::regular_file("assets/icon.svg", b"<svg/>".to_vec()).unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/SKILL.md",
                    b"---\nname: summarize\nversion: 2.1.0\n---\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "vendor/skills/summarize/bin/summarize.py",
                    b"print('summary')\n".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file(
                    "surfaces/index.html",
                    b"<html><body>plan</body></html>".to_vec(),
                )
                .unwrap(),
                AppBundleMember::regular_file("surfaces/app.wasm", b"\0asm".to_vec()).unwrap(),
            ],
            &AppPackageLimits::default(),
        )
        .expect("candidate");
        let package_ref = reference("package-revision:reading-list");
        let bundle = package.bundle_digest().clone();
        assert_eq!(
            runtime.open_scripted_host(
                &package,
                "surfaces/index.html",
                admission(&package_ref, &bundle, reference("bridge:session-wasm")),
                time(1),
            ),
            Err(AppCustomSurfaceRuntimeError::WasmRefused)
        );
    }

    #[test]
    fn worker_run_reply_rejects_wrong_run_and_owner_coordinate_substitution() {
        let expected = reference("run:app-action:expected");
        let mut bridge = message(&reference("bridge:session-1"), "req:run");
        bridge.method = AppBridgeMethod::GetActionRun;
        bridge.view_or_action =
            Some(magician::magician_v2::apps::models::AppName::parse("build").unwrap());
        bridge.payload = serde_json::json!({"run_ref": expected.as_str()});
        let base = serde_json::json!({
            "run_ref": expected.as_str(),
            "status": "running",
            "terminal": false,
            "result_withheld": false,
            "retry_disposition": "poll_run"
        });
        AppCustomSurfaceRuntime::validate_worker_bridge_result(&bridge, &base)
            .expect("correlated reply");
        let mut wrong_run = base.clone();
        wrong_run["run_ref"] = serde_json::json!("run:app-action:other");
        assert!(
            AppCustomSurfaceRuntime::validate_worker_bridge_result(&bridge, &wrong_run).is_err()
        );
        let mut task_leak = base;
        task_leak["task_id"] = serde_json::json!("task-secret");
        assert!(
            AppCustomSurfaceRuntime::validate_worker_bridge_result(&bridge, &task_leak).is_err()
        );
    }

    #[test]
    fn worker_run_reply_preserves_withheld_uncertain_and_generation_truth() {
        let run_ref = reference("run:app-action:expected");
        let withheld = AppCustomSurfaceRunReply {
            run_ref: run_ref.clone(),
            status: AppRunStatus::Completed,
            terminal: true,
            result_withheld: true,
            cancellation_generation: None,
            result: None,
            error: None,
            receipt: None,
            retry_disposition: AppCustomSurfaceRetryDisposition::None,
        };
        withheld.validate_for(&run_ref).expect("withheld");
        let uncertain = AppCustomSurfaceRunReply {
            run_ref: run_ref.clone(),
            status: AppRunStatus::Uncertain,
            terminal: true,
            result_withheld: false,
            cancellation_generation: Some(1),
            result: None,
            error: None,
            receipt: Some(AppCustomSurfaceCancellationReceipt {
                generation: 1,
                idempotency_key: reference("cancel:one"),
                status: AppRunStatus::Uncertain,
                requested_at: time(2),
            }),
            retry_disposition: AppCustomSurfaceRetryDisposition::OutcomeUncertain,
        };
        uncertain.validate_for(&run_ref).expect("uncertain");
        let mut mismatched = uncertain;
        mismatched.cancellation_generation = Some(2);
        assert!(mismatched.validate_for(&run_ref).is_err());
    }

    #[test]
    fn run_wait_bounds_and_unknown_fields_fail_closed() {
        let wait: AppCustomSurfaceRunWaitRequest = serde_json::from_value(serde_json::json!({
            "run_ref": "run:app-action:expected",
            "max_polls": 16,
            "poll_interval_ms": 250
        }))
        .expect("closed wait");
        wait.validate().expect("bounded wait");
        assert!(
            serde_json::from_value::<AppCustomSurfaceRunWaitRequest>(serde_json::json!({
                "run_ref": "run:app-action:expected",
                "max_polls": 1,
                "poll_interval_ms": 25,
                "execution_id": "forbidden"
            }))
            .is_err()
        );
        let unbounded = AppCustomSurfaceRunWaitRequest {
            run_ref: reference("run:app-action:expected"),
            max_polls: CUSTOM_SURFACE_MAX_RUN_POLLS + 1,
            poll_interval_ms: CUSTOM_SURFACE_MIN_RUN_POLL_INTERVAL_MS,
        };
        assert!(unbounded.validate().is_err());
    }

    fn sysinfo_process_gone(pid: u32) -> bool {
        use sysinfo::{Pid, System};
        let mut system = System::new();
        let pid = Pid::from_u32(pid);
        system.refresh_process(pid);
        system.process(pid).is_none()
    }
}
