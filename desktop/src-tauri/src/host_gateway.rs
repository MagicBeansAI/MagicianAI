use crate::config::MagicianDesktopConfig;
use crate::AppState;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, OnceLock,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{sleep, timeout, Duration};
use tracing::{debug, info, warn};

/// Cargo output subpath of the speech helper, joined to `CARGO_TARGET_DIR`
/// when set (dev launches via make targets inherit it). No absolute
/// operator-specific default exists — config and exe-sibling resolution come
/// first, and the last resort is a bare name resolved on PATH so a missing
/// helper fails with a clear spawn error.
const MACOS_SPEECH_HELPER_TARGET_SUBPATH: &str =
    "macos-presence-host/debug/magician-macos-speech-helper";
const DEFAULT_MACOS_PRESENCE_CONTROL_PORT: u16 = 3027;
const HOST_GATEWAY_HEADER_LIMIT: usize = 64 * 1024;
const HOST_GATEWAY_MAX_BODY_BYTES: usize = 40 * 1024 * 1024;
const HOST_GATEWAY_DEFAULT_BODY_BYTES: usize = 512 * 1024;
const HOST_GATEWAY_MAX_CONCURRENT_CONNECTIONS: usize = 32;
const HOST_GATEWAY_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(15);
const HOST_GATEWAY_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
const SPEECH_HELPER_MIN_TIMEOUT: Duration = Duration::from_secs(20);
const SPEECH_HELPER_MAX_TIMEOUT: Duration = Duration::from_secs(55);
const SPEECH_HELPER_BYTES_PER_SECOND_ESTIMATE: usize = 192_000;
const SPEECH_SYNTHESIS_MIN_TIMEOUT: Duration = Duration::from_secs(10);
const SPEECH_SYNTHESIS_MAX_TIMEOUT: Duration = Duration::from_secs(70);
const APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING: usize = 512 * 1024;
const APP_MACOS_TYPED_IDENTITY_HASH_SLOTS: usize = 2;
/// Read-only TCC status. CuaDriver 0.28 raises system prompts only when
/// `prompt` is true; the typed owner never may.
const APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS: &[u8] = br#"{"prompt":false}"#;
/// Private embedded daemon home under the desktop's app-data directory.
const APP_MACOS_TYPED_DAEMON_DIRECTORY: &str = "cua-run";
const APP_MACOS_TYPED_DAEMON_SOCKET: &str = "d.sock";
const APP_MACOS_TYPED_DAEMON_PID_FILE: &str = "d.pid";
/// `sockaddr_un.sun_path` is 104 bytes on macOS, including the NUL.
const APP_MACOS_TYPED_DAEMON_MAX_SOCKET_PATH_BYTES: usize = 103;
const APP_MACOS_TYPED_DAEMON_START_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct HostGatewayState {
    app_macos: crate::app_macos_host::AppMacosHostState,
    app_macos_identity: crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    app_macos_pairing: crate::app_macos_pairing::AppMacosDesktopPairingOwner,
    cua_owner_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GatewayStatus {
    pub enabled: bool,
    pub bind_host: String,
    pub port: u16,
    pub local_url: String,
    pub runtime_url: String,
    pub ui_url: String,
    pub app_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostGatewayStatus {
    pub gateway: GatewayStatus,
}

/// Loopback endpoints used by host-side clients such as the Magicutor browser
/// extension. The gateway remains the stable discovery address; Magician and
/// Magicutor may use non-default ports selected by desktop configuration.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRuntimeEndpoints {
    pub schema_version: u8,
    pub magician_api_base: String,
    pub magician_health_url: String,
    pub magicutor_api_base: String,
    pub magicutor_bridge_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct HostSpeechTranscribeRequest {
    pub audio_b64: String,
    pub content_type: String,
    pub filename: Option<String>,
    pub language: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HostSpeechTranscribeResponse {
    pub transcript: String,
    pub model: String,
    pub language: Option<String>,
    pub extras: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostSpeechSynthesizeRequest {
    pub text: String,
    pub voice: Option<String>,
    pub rate: Option<f32>,
    pub model: Option<String>,
    pub format: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HostSpeechSynthesizeResponse {
    pub audio_b64: String,
    pub content_type: String,
    pub model: String,
    pub voice: Option<String>,
    pub message_id: Option<String>,
    pub extras: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextualAssistOpenRequest {
    pub state: Option<String>,
    pub app: Option<String>,
    pub window_title: Option<String>,
    pub url: Option<String>,
    pub frame_url: Option<String>,
    pub anchor_x: Option<f64>,
    pub anchor_y: Option<f64>,
    pub editable: Option<bool>,
    pub has_selection: Option<bool>,
    pub context_text: Option<String>,
    pub source: Option<String>,
    pub tab_id: Option<i64>,
    pub window_id: Option<i64>,
    pub window_rect: Option<crate::contextual_assist::ContextualAssistCaptureRect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HostSpeechStatus {
    pub available: bool,
    pub stt_available: bool,
    pub tts_available: bool,
    pub helper_path: String,
    pub helper_exists: bool,
    pub stt_authorized: Option<bool>,
    pub stt_authorization_status: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SpeechAuthorizationOutput {
    authorized: bool,
    status: String,
}

#[tauri::command]
pub async fn get_host_gateway_status(app: AppHandle) -> Result<HostGatewayStatus, String> {
    Ok(get_status(&app).await)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppMacosPairingNativeStatus {
    paired: bool,
    owner_approval_required: bool,
    setup_id: Option<String>,
    generation: Option<u64>,
    expires_at_ms: Option<i64>,
    scope_binding_ref: Option<String>,
    requested_targets: Vec<AppMacosPairingNativeReviewTarget>,
    review_material_digest: Option<String>,
    host_identity_digest: Option<String>,
    desktop_identity_digest: Option<String>,
    generation_floor: u64,
    reset_eligible: bool,
    active_actions: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppMacosPairingNativeReviewTarget {
    target_ref: String,
    bundle_id: String,
}

fn app_macos_native_review_material(
    proposal: &magician_app_contract::macos_host::AppMacosHostPairingProposal,
) -> Result<(Vec<AppMacosPairingNativeReviewTarget>, String), String> {
    let targets = proposal
        .requested_targets
        .iter()
        .map(|target| AppMacosPairingNativeReviewTarget {
            target_ref: target.target_ref.clone(),
            bundle_id: target.bundle_id.clone(),
        })
        .collect::<Vec<_>>();
    let canonical = serde_json::to_vec(&json!({
        "schema": "magician.desktop.app-macos-native-review.v1",
        "setup_id": &proposal.setup_id,
        "generation": proposal.generation,
        "scope_binding_ref": &proposal.scope_binding_ref,
        "requested_targets": &proposal.requested_targets,
    }))
    .map_err(|_| "typed macOS native review material is invalid".to_owned())?;
    Ok((
        targets,
        format!("blake3:{}", blake3::hash(&canonical).to_hex()),
    ))
}

#[tauri::command]
pub async fn get_app_macos_host_pairing_status(
    app: AppHandle,
) -> Result<AppMacosPairingNativeStatus, String> {
    ensure_typed_app_macos_pairing_owner(&app).await?;
    let now_ms = current_unix_ms()?;
    let app_state = app.state::<AppState>();
    let state = app_state.host_gateway.lock().await;
    let activity = state.app_macos.activity_status();
    let pending = state.app_macos_pairing.native_pending_review(now_ms);
    let reset_context = state.app_macos_pairing.native_reset_context()?;
    let (host_identity_digest, generation_floor, reset_eligible, desktop_identity_digest) =
        match reset_context {
            Some((host_identity, floor, eligible)) => {
                let (_, _, desktop_digest) = state.app_macos_identity.public_identity()?;
                (Some(host_identity), floor, eligible, Some(desktop_digest))
            },
            None => (None, 0, false, None),
        };
    let (
        setup_id,
        generation,
        expires_at_ms,
        scope_binding_ref,
        requested_targets,
        review_material_digest,
        owner_approval_required,
    ) = match pending {
        Some((proposal, approval_required)) => {
            let (targets, digest) = app_macos_native_review_material(&proposal)?;
            (
                Some(proposal.setup_id),
                Some(proposal.generation),
                Some(proposal.expires_at_ms),
                Some(proposal.scope_binding_ref),
                targets,
                Some(digest),
                approval_required,
            )
        },
        None => (None, None, None, None, Vec::new(), None, false),
    };
    Ok(AppMacosPairingNativeStatus {
        paired: activity.paired,
        owner_approval_required,
        setup_id,
        generation,
        expires_at_ms,
        scope_binding_ref,
        requested_targets,
        review_material_digest,
        host_identity_digest,
        desktop_identity_digest,
        generation_floor,
        reset_eligible,
        active_actions: activity.active_actions,
    })
}

#[tauri::command]
pub async fn reset_app_macos_host_pairing(
    app: AppHandle,
    challenge: magician_app_contract::macos_host::AppMacosHostPairingResetChallenge,
    expected_host_identity_digest: String,
    expected_generation_floor: u64,
    expected_desktop_identity_digest: String,
) -> Result<magician_app_contract::macos_host::AppMacosHostPairingResetAck, String> {
    ensure_typed_app_macos_pairing_owner(&app).await?;
    let _native_approval = Arc::clone(typed_app_macos_native_approval_lock())
        .lock_owned()
        .await;
    let now_ms = current_unix_ms()?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    app_macos_pairing.reset_after_owner_confirmation(
        challenge,
        &expected_host_identity_digest,
        expected_generation_floor,
        &expected_desktop_identity_digest,
        now_ms,
        app_macos,
        app_macos_identity,
    )
}

#[tauri::command]
pub async fn begin_app_macos_host_identity_approval(
    app: AppHandle,
) -> Result<crate::app_macos_identity::AppMacosDesktopIdentityEnrollment, String> {
    ensure_typed_app_macos_pairing_owner(&app).await?;
    app.state::<AppState>()
        .host_gateway
        .lock()
        .await
        .app_macos_identity
        .begin_owner_approval(current_unix_ms()?)
}

#[tauri::command]
pub async fn approve_app_macos_host_pairing(
    app: AppHandle,
    setup_id: String,
    expected_generation: u64,
    expected_review_material_digest: String,
    cua_driver_binary: String,
) -> Result<magician_app_contract::macos_host::AppMacosHostPairingApproval, String> {
    ensure_typed_app_macos_pairing_owner(&app).await?;
    init_typed_app_macos_daemon_home(&app)?;
    let _native_approval = Arc::clone(typed_app_macos_native_approval_lock())
        .lock_owned()
        .await;
    let now_ms = current_unix_ms()?;
    let (proposal, host_identity_digest) = {
        let app_state = app.state::<AppState>();
        let state = app_state.host_gateway.lock().await;
        let proposal = state
            .app_macos_pairing
            .pending_proposal(&setup_id, now_ms)?;
        let (_, current_review_digest) = app_macos_native_review_material(&proposal)?;
        if proposal.generation != expected_generation
            || current_review_digest != expected_review_material_digest
        {
            return Err(
                "typed macOS approval does not match the displayed scope and targets".to_owned(),
            );
        }
        if let Some(approval) = state
            .app_macos_pairing
            .retained_approval(&setup_id, now_ms)?
        {
            return Ok(approval);
        }
        (
            proposal,
            state.app_macos_pairing.host_identity_digest()?.to_owned(),
        )
    };
    let selected_binary = PathBuf::from(cua_driver_binary);
    if !selected_binary.is_absolute()
        || std::fs::symlink_metadata(&selected_binary)
            .map(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Err(
            "select the exact cua-driver executable inside its app bundle \
             (/Applications/CuaDriver.app/Contents/MacOS/cua-driver), not a symlink"
                .to_owned(),
        );
    }
    let owner_stop = crate::app_macos_host::AppMacosHostStopSignal::new();
    let expires_at_ms = proposal.expires_at_ms.min(now_ms.saturating_add(120_000));
    let identity_slot =
        reserve_typed_app_macos_identity_hash_slot(&owner_stop, expires_at_ms).await?;
    let slot = Arc::new(identity_slot);
    let hash_slot = Arc::clone(&slot);
    let hash_binary = selected_binary.clone();
    let artifact_root = app
        .path()
        .app_data_dir()
        .map_err(|_| "typed macOS CUA artifact root is unavailable".to_owned())?
        .join("app-macos-cua-owner-v2");
    let target_requests = proposal.requested_targets.clone();
    let mut identity_observation = tokio::task::spawn_blocking(move || {
        let _identity_hash_slot = hash_slot;
        let (staged_binary, binary_digest) =
            crate::app_macos_host::app_macos_host_stage_binary(&hash_binary, &artifact_root)?;
        let mut reviewed = Vec::with_capacity(target_requests.len());
        for target in target_requests {
            let application_identity_digest =
                crate::app_macos_host::app_macos_host_current_application_identity_digest_blocking(
                    &target.bundle_id,
                    None,
                )?;
            reviewed.push(
                magician_app_contract::macos_host::AppMacosHostPairingTargetIdentity {
                    target_ref: target.target_ref,
                    bundle_id: target.bundle_id,
                    application_identity_digest,
                },
            );
        }
        Some((staged_binary, binary_digest, reviewed))
    });
    let remaining_ms = expires_at_ms
        .checked_sub(current_unix_ms()?)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "typed macOS pairing identity observation expired".to_owned())?;
    let identity_deadline = tokio::time::sleep(Duration::from_millis(
        u64::try_from(remaining_ms)
            .map_err(|_| "typed macOS pairing lifetime is out of range".to_owned())?,
    ));
    tokio::pin!(identity_deadline);
    let (binary, cua_driver_binary_digest, reviewed_targets) = tokio::select! {
        result = &mut identity_observation => {
            result
                .map_err(|_| "typed macOS pairing identity observation was interrupted".to_owned())?
                .ok_or_else(|| "typed macOS pairing could not resolve exact binary/app identities".to_owned())?
        },
        _ = owner_stop.stopped() => {
            return Err("typed macOS pairing identity observation was cancelled".to_owned());
        },
        _ = &mut identity_deadline => {
            return Err("typed macOS pairing identity observation exceeded its deadline".to_owned());
        },
    };
    let permissions = handle_typed_app_macos_ax(
        &binary,
        &cua_driver_binary_digest,
        "check_permissions",
        APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS,
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        &owner_stop,
        expires_at_ms,
    )
    .await?;
    let tcc_policy = typed_app_macos_tcc_policy_observation(&permissions.stdout)?;
    if !permissions.ok || !tcc_policy.accessibility {
        return Err("grant Accessibility to the selected CUA owner before approval".to_owned());
    }
    let key = magician_app_contract::macos_host::decode_pairing_key(&proposal.signing_key_hex)
        .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
    let mut approval = magician_app_contract::macos_host::AppMacosHostPairingApproval {
        schema: magician_app_contract::macos_host::APP_MACOS_HOST_PAIRING_V1.to_owned(),
        setup_id: proposal.setup_id.clone(),
        generation: proposal.generation,
        key_id: proposal.key_id.clone(),
        scope_binding_ref: proposal.scope_binding_ref.clone(),
        proposal_digest: proposal
            .digest()
            .map_err(|_| "typed macOS proposal digest is invalid".to_owned())?,
        gateway_action_url: proposal.gateway_action_url.clone(),
        gateway_endpoint_digest: proposal.gateway_endpoint_digest.clone(),
        host_identity_digest,
        cua_driver_binary_digest,
        tcc_policy_digest: tcc_policy.digest,
        tcc_epoch: 0,
        reviewed_targets,
        approved_at_ms: now_ms,
        expires_at_ms: proposal.expires_at_ms,
        signature: String::new(),
        desktop_identity_key_id: String::new(),
        desktop_identity_signature_hex: String::new(),
    };
    {
        let app_state = app.state::<AppState>();
        let mut state = app_state.host_gateway.lock().await;
        approval.tcc_epoch = state.app_macos_pairing.next_tcc_epoch()?;
        approval = state
            .app_macos_identity
            .sign_approval(approval, &proposal, &key)?;
        state.app_macos_pairing.record_approval(
            &setup_id,
            approval.clone(),
            binary,
            current_unix_ms()?,
        )?;
    }
    Ok(approval)
}

fn typed_app_macos_native_approval_lock() -> &'static Arc<Mutex<()>> {
    static LOCK: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(Mutex::new(())))
}

#[tauri::command]
pub async fn revoke_app_macos_host_pairing(app: AppHandle) -> Result<u64, String> {
    ensure_typed_app_macos_pairing_owner(&app).await?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    app_macos_pairing.revoke(current_unix_ms()?, app_macos, app_macos_identity)
}

pub async fn start_background_services(app: AppHandle) -> Result<(), String> {
    start_gateway_server(app.clone()).await?;
    Ok(())
}

pub async fn get_status(app: &AppHandle) -> HostGatewayStatus {
    let config = cloned_config(app).await;
    HostGatewayStatus {
        gateway: GatewayStatus {
            enabled: config.host_gateway.enabled,
            bind_host: config.host_gateway.bind_host.clone(),
            port: config.host_gateway.port,
            local_url: config.host_gateway.local_url.clone(),
            runtime_url: config.host_gateway.runtime_url.clone(),
            ui_url: config.host_gateway.ui_url.clone(),
            app_url: crate::tray::app_url_from_config(&config),
        },
    }
}

async fn cloned_config(app: &AppHandle) -> MagicianDesktopConfig {
    app.state::<AppState>().config.lock().await.clone()
}

async fn start_gateway_server(app: AppHandle) -> Result<(), String> {
    if let Err(error) = ensure_typed_app_macos_pairing_owner(&app).await {
        warn!(error = %error, "typed Apps macOS host remains unavailable");
    }
    let config = cloned_config(&app).await;
    if !config.host_gateway.enabled {
        info!("Host gateway disabled by config");
        return Ok(());
    }
    if !typed_app_macos_gateway_binding_is_exact(
        &config.host_gateway.bind_host,
        config.host_gateway.port,
    ) {
        return Err(
            "typed Apps macOS routes require the exact 127.0.0.1:3017 host owner".to_owned(),
        );
    }

    let bind_addr = format!(
        "{}:{}",
        config.host_gateway.bind_host, config.host_gateway.port
    );
    let listener = TcpListener::bind(&bind_addr)
        .await
        .map_err(|error| format!("failed to bind host gateway at {}: {}", bind_addr, error))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to inspect typed host gateway listener: {error}"))?;
    if local_addr.ip().to_string() != "127.0.0.1" || local_addr.port() != 3017 {
        return Err("typed Apps macOS listener identity is not exact loopback:3017".to_owned());
    }
    info!(
        "Host gateway listening at {} (runtime URL {})",
        bind_addr, config.host_gateway.runtime_url
    );

    tauri::async_runtime::spawn(async move {
        if let Err(error) = accept_gateway_connections(listener, app).await {
            warn!("Host gateway server stopped: {}", error);
        }
    });
    Ok(())
}

fn typed_app_macos_gateway_binding_is_exact(bind_host: &str, port: u16) -> bool {
    bind_host == "127.0.0.1" && port == 3017
}

async fn accept_gateway_connections(listener: TcpListener, app: AppHandle) -> Result<(), String> {
    let connection_slots = Arc::new(Semaphore::new(HOST_GATEWAY_MAX_CONCURRENT_CONNECTIONS));
    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .map_err(|error| format!("host gateway accept failed: {}", error))?;
        if !peer.ip().is_loopback() {
            warn!(peer = %peer, "rejected non-loopback host gateway connection");
            continue;
        }
        let Some(connection_slot) = try_reserve_host_gateway_connection(&connection_slots) else {
            warn!(peer = %peer, "host gateway connection capacity is saturated");
            continue;
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _connection_slot = connection_slot;
            if let Err(error) = handle_gateway_connection(stream, app).await {
                warn!("Host gateway request failed: {}", error);
            }
        });
    }
}

fn try_reserve_host_gateway_connection(slots: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    Arc::clone(slots).try_acquire_owned().ok()
}

async fn handle_gateway_connection(mut stream: TcpStream, app: AppHandle) -> Result<(), String> {
    let request =
        read_host_gateway_request_with_deadline(&mut stream, HOST_GATEWAY_REQUEST_READ_TIMEOUT)
            .await?;
    let first_line = request.head.lines().next().unwrap_or_default();
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");
    let normalized_path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    let normalized_path = if normalized_path.is_empty() {
        "/"
    } else {
        normalized_path
    };

    match (method, normalized_path) {
        ("GET", "/health") => write_json(&mut stream, 200, "OK", &json!({ "status": "ok" })).await,
        ("POST", "/host/ui/android-observation") => {
            let config = cloned_config(&app).await;
            let trust_mode = path
                .split_once('?')
                .and_then(|(_, query)| query.strip_prefix("trust="));
            let trust_mode_valid = matches!(
                trust_mode,
                Some("play_integrity") | Some("owner_pinned_private_build")
            );
            match trusted_local_settings_origin(&request.head, &config) {
                Some(origin) if request.body.is_empty() && trust_mode_valid => {
                    crate::tray::open_android_observation_approval(&app, trust_mode);
                    write_cors_json(&mut stream, 200, "OK", &json!({ "opened": true }), &origin)
                        .await
                },
                Some(origin) => {
                    write_cors_json(
                        &mut stream,
                        400,
                        "Bad Request",
                        &json!({ "opened": false, "error": "request must contain one reviewed trust mode and an empty body" }),
                        &origin,
                    )
                    .await
                },
                None => write_error(
                    &mut stream,
                    403,
                    "Forbidden",
                    "Android observation setup requires the configured local Web Settings origin",
                )
                .await,
            }
        },
        ("GET", "/host/runtime/endpoints") => {
            let config = cloned_config(&app).await;
            write_json(&mut stream, 200, "OK", &runtime_endpoints(&config)).await
        },
        ("GET", "/host/status") | ("GET", "/host/presence/status") => {
            let status = get_status(&app).await;
            write_json(&mut stream, 200, "OK", &status).await
        },
        ("POST", "/host/speech/transcribe") => {
            match handle_speech_transcribe(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
            }
        },
        ("POST", "/host/speech/synthesize") => {
            match handle_speech_synthesize(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
            }
        },
        ("GET", "/host/speech/status") => {
            let status = get_speech_status(&app).await;
            write_json(&mut stream, 200, "OK", &status).await
        },
        ("POST", "/host/overlay/draw") => match handle_overlay_draw(&app, &request.body).await {
            Ok(()) => write_json(&mut stream, 200, "OK", &json!({ "ok": true })).await,
            Err(error) => write_error(&mut stream, 400, "Bad Request", &error).await,
        },
        ("POST", "/host/contextual-assist/open") => {
            match handle_contextual_assist_open(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 400, "Bad Request", &error).await,
            }
        },
        ("POST", "/host/app/open") => match handle_app_open(&app, &request.body) {
            Ok(()) => write_json(&mut stream, 200, "OK", &json!({ "ok": true })).await,
            Err(error) => write_error(&mut stream, 400, "Bad Request", &error).await,
        },
        ("POST", "/host/reminders/create") => {
            match handle_create_apple_reminder(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
            }
        },
        ("POST", "/host/screen/capture") => match handle_screen_capture(&request.body).await {
            Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
            Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
        },
        ("POST", "/host/applescript") => match handle_applescript(&request.body).await {
            Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
            Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
        },
        ("GET", "/host/automation/status") => {
            write_json(&mut stream, 200, "OK", &automation_status().await).await
        },
        ("POST", "/host/apps/macos/pairing/propose") => {
            match handle_typed_app_macos_pairing_proposal(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 202, "Accepted", &response).await,
                Err(error) => write_error(&mut stream, 409, "Conflict", &error).await,
            }
        },
        ("POST", "/host/apps/macos/pairing/attest") => {
            match handle_typed_app_macos_identity_attestation(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 403, "Forbidden", &error).await,
            }
        },
        ("POST", "/host/apps/macos/pairing/status") => {
            match handle_typed_app_macos_pairing_status(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 403, "Forbidden", &error).await,
            }
        },
        ("POST", "/host/apps/macos/pairing/revoke") => {
            match handle_typed_app_macos_pairing_revoke(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 403, "Forbidden", &error).await,
            }
        },
        ("POST", "/host/apps/macos/pairing/finalize") => {
            match handle_typed_app_macos_pairing_finalization(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 409, "Conflict", &error).await,
            }
        },
        ("POST", "/host/apps/android/owner/status") => {
            match handle_typed_app_android_owner_status(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 403, "Forbidden", &error).await,
            }
        },
        ("POST", "/host/apps/macos/action") => {
            match handle_typed_app_macos_action(&app, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 503, "Service Unavailable", &error).await,
            }
        },
        ("POST", "/host/apps/macos/cancel") => {
            let stopped = app
                .state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .cancel_exact(&request.body)
                .map_err(|error| error.to_string())?;
            if stopped {
                write_json(
                    &mut stream,
                    200,
                    "OK",
                    &json!({ "ok": true, "stopped": true }),
                )
                .await
            } else {
                write_json(
                    &mut stream,
                    409,
                    "Conflict",
                    &json!({ "ok": false, "stopped": false }),
                )
                .await
            }
        },
        ("GET", "/host/apps/macos/status") => {
            let status = app
                .state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .activity_status();
            write_json(&mut stream, 200, "OK", &status).await
        },
        ("POST", "/host/apps/macos/stop") => {
            let stopped = app
                .state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .request_owner_stop();
            write_json(
                &mut stream,
                200,
                "OK",
                &json!({ "ok": true, "stopRequestedActions": stopped }),
            )
            .await
        },
        ("POST", ax_path) if ax_path.starts_with("/host/ax/") => {
            // Proxy an Accessibility (AX) action to the host's `cua-driver`
            // daemon — the container relay (cua_call.py) POSTs the per-action
            // JSON here when no local cua-driver is reachable. Thin passthrough,
            // same posture as `/host/applescript` (no pairing token; the
            // magician side gates on the skill grant before it reaches here).
            let action = ax_path.trim_start_matches("/host/ax/").to_string();
            let cua_owner_lock = app
                .state::<AppState>()
                .host_gateway
                .lock()
                .await
                .cua_owner_lock
                .clone();
            let _cua_owner_guard = cua_owner_lock.lock_owned().await;
            match handle_ax(&action, &request.body).await {
                Ok(response) => write_json(&mut stream, 200, "OK", &response).await,
                Err(error) => write_error(&mut stream, 500, "Internal Server Error", &error).await,
            }
        },
        ("GET", "/host/presence/start")
        | ("GET", "/host/presence/stop")
        | ("GET", "/host/presence/restart") => {
            write_error(
                &mut stream,
                405,
                "Method Not Allowed",
                "use POST for process mutations",
            )
            .await
        },
        _ => {
            write_error(
                &mut stream,
                404,
                "Not Found",
                "unknown host gateway endpoint",
            )
            .await
        },
    }
}

fn runtime_endpoints(config: &MagicianDesktopConfig) -> HostRuntimeEndpoints {
    HostRuntimeEndpoints {
        schema_version: 1,
        magician_api_base: config.engine_url("/api/magician/v2"),
        magician_health_url: config.engine_url("/health"),
        magicutor_api_base: format!("http://127.0.0.1:{}", config.network.magicutor_port),
        magicutor_bridge_url: format!(
            "ws://127.0.0.1:{}/bridge/native",
            config.network.magicutor_port
        ),
    }
}

/// Route a presence-host deep link to the browser, except Attention and
/// approval paths, which use the bounded native Attention window. Dispatch on
/// the main thread because the Attention branch may perform window operations.
fn handle_app_open(app: &AppHandle, body: &[u8]) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct AppOpenRequest {
        path: String,
    }
    let request: AppOpenRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid app-open payload: {error}"))?;
    let path = request.path.trim().to_string();
    if path.is_empty() {
        return Err("empty path".to_string());
    }
    let app_for_main = app.clone();
    app.run_on_main_thread(move || {
        crate::tray::open_app_at_path(&app_for_main, &path);
    })
    .map_err(|error| format!("failed to open app destination: {error}"))
}

#[derive(Debug, Clone, Deserialize)]
struct HostAppleReminderRequest {
    idempotency_key: String,
    title: String,
    notes: String,
    at: String,
    #[serde(default)]
    timezone: Option<String>,
}

#[derive(Debug, Serialize)]
struct HostAppleReminderResponse {
    reminder_id: String,
    provider: &'static str,
    app_opened: bool,
    replayed: bool,
}

fn validate_apple_reminder_request(request: &HostAppleReminderRequest) -> Result<(), String> {
    if request.idempotency_key.trim().is_empty() || request.idempotency_key.chars().count() > 512 {
        return Err(
            "reminder idempotency_key must be non-empty and contain at most 512 characters"
                .to_string(),
        );
    }
    if request.title.trim().is_empty() {
        return Err("reminder title is required".to_string());
    }
    if request.title.chars().count() > 200 {
        return Err("reminder title exceeds 200 characters".to_string());
    }
    if request.notes.chars().count() > 4_000 {
        return Err("reminder notes exceed 4000 characters".to_string());
    }
    if request.at.trim().is_empty() || request.at.chars().count() > 80 {
        return Err("reminder at must be a bounded RFC3339 date-time".to_string());
    }
    if request
        .timezone
        .as_deref()
        .is_some_and(|timezone| timezone.chars().count() > 80)
    {
        return Err("reminder timezone exceeds 80 characters".to_string());
    }
    Ok(())
}

/// Create the reminder through Reminders' macOS scripting interface and bring
/// the app forward. Values travel as argv, never interpolated source, so card
/// text cannot become executable JXA.
async fn handle_create_apple_reminder(
    app: &AppHandle,
    body: &[u8],
) -> Result<HostAppleReminderResponse, String> {
    let request: HostAppleReminderRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid Apple Reminder payload: {error}"))?;
    validate_apple_reminder_request(&request)?;
    let state = app.state::<crate::AppState>();
    let _operation_guard = state.host_gateway.lock().await;
    let mut receipts = load_apple_reminder_receipts(app).await?;
    if let Some(reminder_id) = receipts.get(request.idempotency_key.trim()) {
        activate_reminders().await?;
        return Ok(HostAppleReminderResponse {
            reminder_id: reminder_id.clone(),
            provider: "apple_reminders_macos",
            app_opened: true,
            replayed: true,
        });
    }

    const SOURCE: &str = r#"
function run(argv) {
  const title = argv[0];
  const notes = argv[1];
  const due = new Date(argv[2]);
  if (!Number.isFinite(due.getTime())) throw new Error("invalid reminder date");
  const app = Application("Reminders");
  const lists = app.lists();
  if (!lists.length) throw new Error("Reminders has no writable list");
  let list;
  try { list = app.defaultList(); } catch (_) { list = lists[0]; }
  if (!list) list = lists[0];
  const reminder = app.Reminder({
    name: title,
    body: notes,
    dueDate: due,
    remindMeDate: due
  });
  list.reminders.push(reminder);
  app.activate();
  return reminder.id();
}
"#;
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(SOURCE)
        .arg("--")
        .arg(request.title.trim())
        .arg(request.notes.trim())
        .arg(request.at.trim())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match timeout(Duration::from_secs(20), command.output()).await {
        Ok(result) => {
            result.map_err(|error| format!("failed to run Reminders automation: {error}"))?
        },
        Err(_) => return Err("Reminders automation timed out after 20s".to_string()),
    };
    if !output.status.success() {
        return Err(format!(
            "Reminders automation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let reminder_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if reminder_id.is_empty() {
        return Err("Reminders automation returned no reminder identifier".to_string());
    }
    receipts.insert(
        request.idempotency_key.trim().to_string(),
        reminder_id.clone(),
    );
    persist_apple_reminder_receipts(app, &receipts).await?;
    Ok(HostAppleReminderResponse {
        reminder_id,
        provider: "apple_reminders_macos",
        app_opened: true,
        replayed: false,
    })
}

async fn activate_reminders() -> Result<(), String> {
    let output = timeout(
        Duration::from_secs(10),
        Command::new("/usr/bin/open")
            .arg("-a")
            .arg("Reminders")
            .output(),
    )
    .await
    .map_err(|_| "opening Reminders timed out after 10s".to_string())?
    .map_err(|error| format!("failed to open Reminders: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "failed to open Reminders: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn apple_reminder_receipts_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|directory| directory.join("apple-reminder-receipts.json"))
        .map_err(|error| format!("failed to resolve reminder receipt directory: {error}"))
}

async fn load_apple_reminder_receipts(app: &AppHandle) -> Result<HashMap<String, String>, String> {
    let path = apple_reminder_receipts_path(app)?;
    match tokio::fs::read(&path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("failed to decode reminder receipts: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(format!("failed to read reminder receipts: {error}")),
    }
}

async fn persist_apple_reminder_receipts(
    app: &AppHandle,
    receipts: &HashMap<String, String>,
) -> Result<(), String> {
    let path = apple_reminder_receipts_path(app)?;
    let parent = path
        .parent()
        .ok_or_else(|| "reminder receipt path has no parent".to_string())?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("failed to create reminder receipt directory: {error}"))?;
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec(receipts)
        .map_err(|error| format!("failed to encode reminder receipts: {error}"))?;
    tokio::fs::write(&temporary, bytes)
        .await
        .map_err(|error| format!("failed to write reminder receipts: {error}"))?;
    tokio::fs::rename(&temporary, &path)
        .await
        .map_err(|error| format!("failed to commit reminder receipts: {error}"))
}

// ---------------------------------------------------------------------------
// macOS automation routes (screen capture / AppleScript / AX).
//
// These run macOS-only operations with the desktop app's TCC grants on behalf
// of magician — native OR container, reached via MAGICIAN_HOST_GATEWAY_URL.
// magician's `HostAutomationProvider` is the client; this is the one code path
// for both flows.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
struct ScreenCaptureRequest {
    #[serde(default)]
    display: Option<u32>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    window: bool,
}

#[derive(serde::Serialize)]
struct ScreenCaptureResponse {
    image_b64: String,
    content_type: String,
    width: Option<u32>,
    height: Option<u32>,
}

/// `screencapture(1)` to a temp PNG, returned base64. TCC: Screen Recording.
async fn handle_screen_capture(body: &[u8]) -> Result<ScreenCaptureResponse, String> {
    let request: ScreenCaptureRequest = if body.is_empty() {
        ScreenCaptureRequest::default()
    } else {
        serde_json::from_slice(body)
            .map_err(|error| format!("invalid screen-capture payload: {error}"))?
    };
    let out_path = temp_automation_path("png")?;
    let mut command = Command::new("/usr/sbin/screencapture");
    command.arg("-x"); // silent (no shutter sound)
    if let Some(display) = request.display {
        command.arg("-D").arg(display.to_string());
    }
    if let Some(region) = request
        .region
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.arg("-R").arg(region);
    }
    // `window` is reserved for a future window-id capture path; today every
    // capture is full-display (or region), matching the screen-observation rail.
    let _ = request.window;
    command.arg(&out_path);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match timeout(Duration::from_secs(15), command.output()).await {
        Ok(result) => result.map_err(|error| format!("failed to run screencapture: {error}"))?,
        Err(_) => return Err("screencapture timed out after 15s".to_string()),
    };
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&out_path).await;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!(
            "screencapture failed status={} stderr={stderr}",
            output.status
        ));
    }
    let bytes = tokio::fs::read(&out_path)
        .await
        .map_err(|error| format!("failed to read screen capture: {error}"))?;
    let _ = tokio::fs::remove_file(&out_path).await;
    if bytes.is_empty() {
        return Err("screencapture produced an empty image".to_string());
    }
    Ok(ScreenCaptureResponse {
        image_b64: BASE64.encode(&bytes),
        content_type: "image/png".to_string(),
        width: None,
        height: None,
    })
}

#[derive(serde::Deserialize)]
struct AppleScriptRequest {
    source: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

#[derive(serde::Serialize)]
struct AppleScriptResult {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// `osascript(1)` (AppleScript or JXA). TCC: Automation. Broadest route — the
/// magician side gates this on the skill grant before it ever reaches here.
async fn handle_applescript(body: &[u8]) -> Result<AppleScriptResult, String> {
    let request: AppleScriptRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid applescript payload: {error}"))?;
    if request.source.trim().is_empty() {
        return Err("empty applescript source".to_string());
    }
    let mut command = Command::new("/usr/bin/osascript");
    if let Some(language) = request
        .language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if language.eq_ignore_ascii_case("javascript") || language.eq_ignore_ascii_case("jxa") {
            command.arg("-l").arg("JavaScript");
        }
    }
    command.arg("-e").arg(&request.source);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let secs = request.timeout_secs.unwrap_or(30).clamp(1, 120);
    let output = match timeout(Duration::from_secs(secs), command.output()).await {
        Ok(result) => result.map_err(|error| format!("failed to run osascript: {error}"))?,
        // The script is killed on timeout (kill_on_drop), so nothing runs
        // after this answer. The usual cause of a script that never returns
        // is macOS holding it on an Automation consent prompt — say so, so a
        // late approval is followed by a retry rather than a dead end.
        Err(_) => {
            return Err(format!(
                "osascript timed out after {secs}s and was stopped; nothing ran after the timeout. \
                 If macOS is showing a permission prompt (\"… wants access to control …\"), \
                 approve it and run the script again."
            ))
        },
    };
    Ok(AppleScriptResult {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Availability for magician's `gateway_available()` probe. `available`
/// accepts either a usable local CuaDriver session on any desktop OS or the
/// native macOS screen/Apple Events route. The detailed fields let callers
/// select the supported route without treating Linux or Windows as macOS.
/// Whether this process may actually drive Reminders.
///
/// The binary existing on disk says nothing about permission: sending an Apple
/// Event additionally needs the `com.apple.security.automation.apple-events`
/// entitlement, an `NSAppleEventsUsageDescription` in the bundle, and a TCC
/// grant for this app. When any of those is missing the send fails with -1743
/// and no prompt is shown, so the only honest check is to attempt one.
///
/// The probe is read-only (it reads the app's name, creating nothing). On a
/// first run it may raise the standard Automation prompt, which is the desired
/// outcome — that prompt is what writes the TCC grant.
async fn apple_events_status() -> serde_json::Value {
    const PROBE: &str = r#"Application("Reminders").name()"#;
    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(PROBE)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(Duration::from_secs(10), command.output()).await {
        Ok(Ok(output)) if output.status.success() => json!({
            "permitted": true,
            "reason": serde_json::Value::Null,
        }),
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            // -1743 is errAEEventNotPermitted: denied, or never granted because
            // the prompt could not be shown.
            let denied = stderr.contains("-1743");
            json!({
                "permitted": false,
                "reason": if denied {
                    "not permitted to control Reminders (-1743): grant Automation                      access, and check the app is signed with the apple-events                      entitlement and an NSAppleEventsUsageDescription"
                        .to_string()
                } else {
                    stderr
                },
            })
        },
        Ok(Err(error)) => json!({
            "permitted": false,
            "reason": format!("failed to run the Apple Events probe: {error}"),
        }),
        Err(_) => json!({
            "permitted": false,
            "reason": "Apple Events probe timed out after 10s",
        }),
    }
}

fn automation_is_available(
    platform: &str,
    cua_available: bool,
    screen_capture: bool,
    apple_events_ok: bool,
) -> bool {
    cua_available || (platform == "macos" && screen_capture && apple_events_ok)
}

/// This app's macOS Accessibility (TCC) grant. Other platforms have no
/// per-app grant to report: Linux input rides CuaDriver's AT-SPI session and
/// Windows its signed-in desktop, so `permitted` is `null` there rather than
/// a `true` nothing checked.
fn accessibility_status(platform: &str, trusted: bool) -> serde_json::Value {
    if platform != "macos" {
        return json!({
            "permitted": serde_json::Value::Null,
            "reason": "not applicable: Accessibility is a macOS permission",
        });
    }
    json!({
        "permitted": trusted,
        "reason": if trusted {
            serde_json::Value::Null
        } else {
            json!("this app is not trusted for Accessibility: System Events keystrokes and clicks fail (error 1002) until it is enabled in System Settings → Privacy & Security → Accessibility; re-signing the app resets the grant")
        },
    })
}

async fn automation_status() -> serde_json::Value {
    let screen_capture = std::path::Path::new("/usr/sbin/screencapture").is_file();
    let osascript_present = std::path::Path::new("/usr/bin/osascript").is_file();
    let cua_driver = resolve_cua_driver_binary().is_some();
    // Installed and able to start in this desktop session; OS permission
    // verification is a separate setup check, independent of Apple Events.
    let cua_available = cua_driver && runtime_core::cua::has_desktop_session();
    let apple_events = if osascript_present {
        apple_events_status().await
    } else {
        json!({ "permitted": false, "reason": "/usr/bin/osascript is missing" })
    };
    let apple_events_ok = apple_events
        .get("permitted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let native_automation_available =
        std::env::consts::OS == "macos" && screen_capture && apple_events_ok;
    // Apple Events alone let a script drive an app; a `System Events`
    // keystroke or click also needs this app's Accessibility grant, which a
    // re-sign resets. Without it the keystroke fails mid-task with error
    // 1002, so the missing grant is reported here, up front. It does not
    // gate `available`: scripts that send no input still run.
    let accessibility = accessibility_status(
        std::env::consts::OS,
        crate::voice_gesture::accessibility_trusted(),
    );
    json!({
        // Keep the legacy aggregate useful on every desktop OS. macOS can use
        // its native screen/Apple Events lane, while Windows and Linux become
        // available through their local CuaDriver desktop session.
        "available": automation_is_available(
            std::env::consts::OS,
            cua_available,
            screen_capture,
            apple_events_ok,
        ),
        "native_automation_available": native_automation_available,
        "screen_capture": screen_capture,
        "applescript": osascript_present,
        "apple_events": apple_events,
        "accessibility": accessibility,
        "cua_driver": cua_driver,
        "cua_available": cua_available,
        "platform": std::env::consts::OS,
    })
}

// ---------------------------------------------------------------------------
// AX (Accessibility) proxy to the host's `cua-driver` daemon.
//
// CuaDriver supports macOS, Windows and Linux desktop sessions through a local
// daemon (Unix socket or Windows named pipe). This handler shells the binary on the
// host on behalf of magician (native OR container, reached via the gateway).
//
// It mirrors `skillshub/macos-ui-automation/bin/macos-ui-controller`: ensure the
// shared daemon (started via `open -n -g -a CuaDriver --args serve` so it
// inherits the .app's Accessibility + Screen-Recording TCC grants) and invoke
// `cua-driver call <action> <args_json>`. CuaDriver 0.28 element addresses are
// scoped to the snapshot that minted them, so a stale-address failure is never
// retried with the same arguments; the reply carries a re-snapshot hint.
//
// The typed Apps owner further below does NOT use this shared daemon: it runs
// its own private embedded daemon on a private socket.
// ---------------------------------------------------------------------------

const CUA_DRIVER_DAEMON_START_TIMEOUT: Duration = Duration::from_secs(6);
const CUA_DRIVER_CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(serde::Serialize)]
struct AxResult {
    ok: bool,
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Use shared discovery, including the official Windows installer location.
fn resolve_cua_driver_binary() -> Option<PathBuf> {
    runtime_core::cua::driver_binary()
}

/// Is the cua-driver daemon running? Mirrors `cua_call.py::daemon_status` —
/// `cua-driver status` exits 0 when a daemon is up on the socket.
pub(crate) async fn cua_driver_daemon_running(binary: &PathBuf) -> bool {
    let mut command = Command::new(binary);
    command
        .arg("status")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(Duration::from_secs(5), command.output()).await {
        Ok(Ok(output)) => output.status.success(),
        _ => false,
    }
}

/// Ensure the cua-driver daemon is up. Starting via `open -n -g -a CuaDriver`
/// is the supported path — the `.app` owns the TCC grants (Accessibility +
/// Screen Recording), which a shell-spawned `cua-driver serve` would NOT carry.
/// Falls back to a backgrounded `cua-driver serve` only when the .app is
/// unavailable (matches `cua_call.py::ensure_daemon`).
pub(crate) async fn ensure_cua_driver_daemon(binary: &PathBuf) -> bool {
    if cua_driver_daemon_running(binary).await {
        return true;
    }

    if !runtime_core::cua::has_desktop_session() {
        return false;
    }

    let mut started = false;
    if cfg!(target_os = "macos") && std::path::Path::new("/Applications/CuaDriver.app").is_dir() {
        let mut open = Command::new("/usr/bin/open");
        open.args(["-n", "-g", "-a", "CuaDriver", "--args", "serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Ok(Ok(status)) = timeout(Duration::from_secs(10), open.status()).await {
            started = status.success();
        }
    }

    if started && cua_driver_wait_until_running(binary).await {
        return true;
    }

    if !started {
        // Normal Windows/Linux startup. On macOS this fallback carries the
        // caller's TCC identity when the app bundle is unavailable.
        let mut serve = Command::new(binary);
        serve
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        #[cfg(target_os = "windows")]
        {
            serve.creation_flags(0x00000008 | 0x00000200); // detached, new process group
        }
        let _ = serve.spawn();
        if cua_driver_wait_until_running(binary).await {
            return true;
        }
    }

    cua_driver_daemon_running(binary).await
}

async fn cua_driver_wait_until_running(binary: &PathBuf) -> bool {
    let deadline = Instant::now() + CUA_DRIVER_DAEMON_START_TIMEOUT;
    while Instant::now() < deadline {
        if cua_driver_daemon_running(binary).await {
            return true;
        }
        sleep(Duration::from_millis(350)).await;
    }
    false
}

/// Run `cua-driver call <action> <args_json>`. `args_json` is the raw JSON
/// object string for the tool's inputSchema (the cua-driver `call` CLI takes it
/// as a single positional argument — exactly `cua_call.py::call_cua`).
async fn run_cua_driver_call(
    binary: &PathBuf,
    action: &str,
    args_json: &str,
) -> Result<std::process::Output, String> {
    let mut command = Command::new(binary);
    command
        .arg("call")
        .arg(action)
        .arg(args_json)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(CUA_DRIVER_CALL_TIMEOUT, command.output()).await {
        Ok(result) => result.map_err(|error| format!("failed to run cua-driver: {error}")),
        Err(_) => Err(format!(
            "cua-driver call timed out after {}s",
            CUA_DRIVER_CALL_TIMEOUT.as_secs()
        )),
    }
}

struct AppMacosTypedProcessOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Where the typed owner's private daemon lives and which host it declares.
/// Set once from the desktop's own app-data directory and bundle identifier.
struct TypedAppMacosDaemonHome {
    directory: PathBuf,
    host_bundle_id: String,
}

/// The typed Apps owner's PRIVATE embedded CuaDriver daemon. It is never
/// CuaDriver.app's shared daemon (the skill relay's): it is spawned by this
/// desktop from the pinned, digest-verified staged bundle with
/// `serve --embedded`, so it inherits the desktop's own Accessibility and
/// Screen Recording grants and never prompts or relaunches, and it listens
/// only on a socket inside a 0700 app-data directory. Its stdin is a lifetime
/// pipe (`CUA_DRIVER_PARENT_LIVENESS_STDIN`): when this process exits, is
/// killed or replaces the daemon for another approved digest, the pipe closes
/// and the daemon shuts down.
struct TypedAppMacosPrivateDaemon {
    binary_digest: String,
    child: tokio::process::Child,
}

fn typed_app_macos_daemon_home() -> &'static OnceLock<TypedAppMacosDaemonHome> {
    static HOME: OnceLock<TypedAppMacosDaemonHome> = OnceLock::new();
    &HOME
}

fn typed_app_macos_private_daemon() -> &'static Mutex<Option<TypedAppMacosPrivateDaemon>> {
    static DAEMON: OnceLock<Mutex<Option<TypedAppMacosPrivateDaemon>>> = OnceLock::new();
    DAEMON.get_or_init(|| Mutex::new(None))
}

pub(crate) fn init_typed_app_macos_daemon_home(app: &AppHandle) -> Result<(), String> {
    if typed_app_macos_daemon_home().get().is_some() {
        return Ok(());
    }
    let directory = app
        .path()
        .app_data_dir()
        .map_err(|_| "typed macOS private daemon home is unavailable".to_owned())?
        .join(APP_MACOS_TYPED_DAEMON_DIRECTORY);
    let _ = typed_app_macos_daemon_home().set(TypedAppMacosDaemonHome {
        directory,
        host_bundle_id: app.config().identifier.clone(),
    });
    Ok(())
}

/// Create (or re-check) the private 0700 daemon directory and return its
/// socket and pid-file paths.
#[cfg(unix)]
fn typed_app_macos_daemon_paths() -> Result<(PathBuf, PathBuf, String), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};

    let home = typed_app_macos_daemon_home()
        .get()
        .ok_or_else(|| "typed macOS private daemon home is not initialized".to_owned())?;
    let parent = home
        .directory
        .parent()
        .ok_or_else(|| "typed macOS private daemon home has no parent".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|_| "typed macOS app-data directory is unavailable".to_owned())?;
    match std::fs::symlink_metadata(&home.directory) {
        Ok(_) => {},
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(false)
                .mode(0o700)
                .create(&home.directory)
                .map_err(|_| {
                    "typed macOS private daemon directory could not be created".to_owned()
                })?;
        },
        Err(_) => return Err("typed macOS private daemon directory is unreadable".to_owned()),
    }
    let metadata = std::fs::symlink_metadata(&home.directory)
        .map_err(|_| "typed macOS private daemon directory is unreadable".to_owned())?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|_| "typed macOS app-data directory is unreadable".to_owned())?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != parent_metadata.uid()
    {
        return Err("typed macOS private daemon directory is not owner-only".to_owned());
    }
    let socket = home.directory.join(APP_MACOS_TYPED_DAEMON_SOCKET);
    if socket.as_os_str().len() > APP_MACOS_TYPED_DAEMON_MAX_SOCKET_PATH_BYTES {
        return Err(
            "typed macOS private daemon socket path exceeds the macOS socket-path limit".to_owned(),
        );
    }
    Ok((
        socket,
        home.directory.join(APP_MACOS_TYPED_DAEMON_PID_FILE),
        home.host_bundle_id.clone(),
    ))
}

#[cfg(not(unix))]
fn typed_app_macos_daemon_paths() -> Result<(PathBuf, PathBuf, String), String> {
    Err("typed macOS private daemon requires macOS".to_owned())
}

/// Curated child environment for every typed CuaDriver process. Nothing from
/// the desktop's own environment (HOME, PATH, provider keys, CUA_* overrides)
/// reaches the owner; telemetry and its stderr notice are off.
fn typed_app_macos_cua_command(executable: &std::path::Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0");
    command
}

/// `cua-driver status --socket <private socket>` exits 0 only when a daemon
/// answers on exactly that socket.
async fn typed_app_macos_private_daemon_answers(
    executable: &std::path::Path,
    socket: &std::path::Path,
    pid_file: &std::path::Path,
) -> bool {
    let mut command = typed_app_macos_cua_command(executable);
    command
        .arg("status")
        .arg("--socket")
        .arg(socket)
        .arg("--pid-file")
        .arg(pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    matches!(
        timeout(Duration::from_secs(5), command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Start the private daemon lazily for the exact approved binary digest, or
/// confirm the running one still answers. A daemon started from another
/// digest (re-pairing, driver upgrade) is replaced, never reused.
async fn ensure_typed_app_macos_private_daemon(
    binary: &PathBuf,
    binary_digest: &str,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<PathBuf, String> {
    let (socket, pid_file, host_bundle_id) = typed_app_macos_daemon_paths()?;
    let mut daemon = typed_app_macos_private_daemon().lock().await;
    if let Some(running) = daemon.as_mut() {
        let alive = matches!(running.child.try_wait(), Ok(None));
        if alive && running.binary_digest == binary_digest {
            let executable =
                crate::app_macos_host::app_macos_host_verify_executable(binary, binary_digest)
                    .ok_or_else(|| {
                        "typed macOS CUA executable is stale or unavailable".to_owned()
                    })?;
            if typed_app_macos_private_daemon_answers(executable.command_path(), &socket, &pid_file)
                .await
            {
                return Ok(socket);
            }
        }
    }
    // Dropping the handle closes the lifetime pipe and kills the child.
    if let Some(mut previous) = daemon.take() {
        let _ = previous.child.start_kill();
        let _ = timeout(Duration::from_secs(2), previous.child.wait()).await;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
        // A socket left by a daemon that died with a previous desktop run.
        // Only an owner-held socket inside the private directory is removed.
        if let Ok(metadata) = std::fs::symlink_metadata(&socket) {
            let parent_uid = socket
                .parent()
                .and_then(|parent| std::fs::symlink_metadata(parent).ok())
                .map(|parent| parent.uid());
            if !metadata.file_type().is_socket() || Some(metadata.uid()) != parent_uid {
                return Err("typed macOS private daemon socket path is unsafe".to_owned());
            }
            let _ = std::fs::remove_file(&socket);
        }
    }
    let executable = crate::app_macos_host::app_macos_host_verify_executable(binary, binary_digest)
        .ok_or_else(|| "typed macOS CUA executable is stale or unavailable".to_owned())?;
    let mut command = typed_app_macos_cua_command(executable.command_path());
    // TODO(cua-bounded-mode): run `--permission-mode bounded` with a
    // `--capability-manifest` (+ `--approve-capability-manifest`) limited to
    // exactly check_permissions, list_apps, list_windows, launch_app,
    // bring_to_front, get_window_state, click, double_click, type_text,
    // press_key, scroll and drag. CuaDriver 0.28.2 documents the manifest only
    // as a "narrow-only tool/resource ceiling" with no published schema, so a
    // guessed format could silently widen or brick the owner; standard mode
    // plus this host's closed verb allowlist is the enforced ceiling today.
    command
        .args(["serve", "--embedded", "--no-overlay"])
        .arg("--socket")
        .arg(&socket)
        .arg("--pid-file")
        .arg(&pid_file)
        .arg("--host-bundle-id")
        .arg(&host_bundle_id)
        .args(["--permission-mode", "standard"])
        .env("CUA_DRIVER_EMBEDDED", "1")
        .env("CUA_DRIVER_PARENT_LIVENESS_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("failed to start the typed macOS private daemon: {error}"))?;
    let mut started = TypedAppMacosPrivateDaemon {
        binary_digest: binary_digest.to_owned(),
        child,
    };
    let now_ms = current_unix_ms()?;
    let remaining_ms = expires_at_ms
        .checked_sub(now_ms)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "typed macOS permit expired before the private daemon started".to_owned())?;
    let deadline = Instant::now()
        + APP_MACOS_TYPED_DAEMON_START_TIMEOUT.min(Duration::from_millis(
            u64::try_from(remaining_ms).unwrap_or(u64::MAX),
        ));
    loop {
        if stop_signal.is_stopped() {
            return Err("typed macOS private daemon start was cancelled".to_owned());
        }
        if !matches!(started.child.try_wait(), Ok(None)) {
            return Err("typed macOS private daemon exited during startup".to_owned());
        }
        if typed_app_macos_private_daemon_answers(executable.command_path(), &socket, &pid_file)
            .await
        {
            *daemon = Some(started);
            return Ok(socket);
        }
        if Instant::now() >= deadline {
            let _ = started.child.start_kill();
            return Err("typed macOS private daemon did not answer on its socket".to_owned());
        }
        sleep(Duration::from_millis(150)).await;
    }
}

/// Apps-only process runner. Unlike `Command::output`, both pipes share one
/// preallocated byte budget while the process is still running. Cancellation,
/// permit expiry, timeout, reader failure and budget exhaustion all kill the
/// child; `kill_on_drop` covers task teardown before explicit cleanup.
#[allow(clippy::too_many_arguments)]
async fn run_typed_cua_driver_call(
    binary: &PathBuf,
    binary_digest: &str,
    socket: &std::path::Path,
    action: &'static str,
    args_json: &str,
    output_byte_ceiling: usize,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<AppMacosTypedProcessOutput, String> {
    if output_byte_ceiling == 0 {
        return Err("typed macOS subprocess has no output budget".to_owned());
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or_else(|| "system clock is outside the typed macOS range".to_owned())?;
    let remaining_ms = expires_at_ms
        .checked_sub(now_ms)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "typed macOS permit expired before subprocess I/O".to_owned())?;
    let process_timeout = CUA_DRIVER_CALL_TIMEOUT
        .min(Duration::from_millis(u64::try_from(remaining_ms).map_err(
            |_| "typed macOS permit lifetime is out of range".to_owned(),
        )?));
    let executable = crate::app_macos_host::app_macos_host_verify_executable(binary, binary_digest)
        .ok_or_else(|| "typed macOS CUA executable is stale or unavailable".to_owned())?;
    // CuaDriver 0.28 `call` always goes through a daemon (`--no-daemon` is
    // gone). `--socket` pins the private embedded daemon; with the curated
    // environment no HOME/default-socket discovery can reach the shared one.
    let mut command = typed_app_macos_cua_command(executable.command_path());
    command
        .arg("call")
        .arg(action)
        .arg(args_json)
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start typed cua-driver: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "typed cua-driver stdout was not captured".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "typed cua-driver stderr was not captured".to_owned())?;
    let remaining = Arc::new(AtomicUsize::new(output_byte_ceiling));
    let stdout_remaining = Arc::clone(&remaining);
    let stderr_remaining = Arc::clone(&remaining);
    let mut readers = tokio::spawn(async move {
        tokio::try_join!(
            read_typed_app_macos_pipe(stdout, stdout_remaining),
            read_typed_app_macos_pipe(stderr, stderr_remaining),
        )
    });
    let deadline = tokio::time::sleep(process_timeout);
    tokio::pin!(deadline);
    let mut completed_readers = None;
    let status = loop {
        tokio::select! {
            status = child.wait() => {
                break status.map_err(|error| format!("failed waiting for typed cua-driver: {error}"))?;
            },
            result = &mut readers, if completed_readers.is_none() => {
                match result {
                    Ok(Ok(output)) => completed_readers = Some(output),
                    Ok(Err(error)) => {
                        terminate_typed_app_macos_child(&mut child).await;
                        return Err(error);
                    },
                    Err(_) => {
                        terminate_typed_app_macos_child(&mut child).await;
                        return Err("typed cua-driver output reader was interrupted".to_owned());
                    },
                }
            },
            _ = stop_signal.stopped() => {
                terminate_typed_app_macos_child(&mut child).await;
                readers.abort();
                return Err("typed cua-driver was cancelled".to_owned());
            },
            _ = &mut deadline => {
                terminate_typed_app_macos_child(&mut child).await;
                readers.abort();
                return Err("typed cua-driver exceeded its permit deadline".to_owned());
            },
        }
    };
    let (stdout, stderr) = match completed_readers {
        Some(output) => output,
        None => tokio::select! {
            result = &mut readers => {
                result
                    .map_err(|_| "typed cua-driver output reader was interrupted".to_owned())??
            },
            _ = stop_signal.stopped() => {
                readers.abort();
                return Err("typed cua-driver output drain was cancelled".to_owned());
            },
            _ = &mut deadline => {
                readers.abort();
                return Err("typed cua-driver output drain exceeded its permit deadline".to_owned());
            },
        },
    };
    Ok(AppMacosTypedProcessOutput {
        status,
        stdout,
        stderr,
    })
}

async fn read_typed_app_macos_pipe<R>(
    mut reader: R,
    remaining: Arc<AtomicUsize>,
) -> Result<Vec<u8>, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let bytes = reader
            .read(&mut buffer)
            .await
            .map_err(|_| "failed to read bounded typed cua-driver output".to_owned())?;
        if bytes == 0 {
            return Ok(output);
        }
        if remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |available| {
                available.checked_sub(bytes)
            })
            .is_err()
        {
            return Err("typed cua-driver exceeded its output byte ceiling".to_owned());
        }
        output.extend_from_slice(&buffer[..bytes]);
    }
}

async fn terminate_typed_app_macos_child(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = timeout(Duration::from_secs(2), child.wait()).await;
}

/// Handle the daemon-lifecycle actions (`serve`/`status`/`stop`) the same way
/// `cua_call.py::main` does — these are top-level cua-driver subcommands rather
/// than `call <tool>`. `doctor` is also a CLI subcommand in CuaDriver 0.28 (not
/// an MCP tool) and runs as `cua-driver doctor --json` without a daemon.
/// Returns `Some(..)` when `action` is handled here, `None` otherwise so the
/// caller falls through to `call`.
async fn handle_ax_lifecycle(binary: &PathBuf, action: &str) -> Option<Result<AxResult, String>> {
    match action {
        "serve" => {
            let running = ensure_cua_driver_daemon(binary).await;
            Some(Ok(AxResult {
                ok: running,
                stdout: json!({ "running": running, "started_or_already_running": running })
                    .to_string(),
                stderr: String::new(),
                exit_code: if running { 0 } else { 1 },
            }))
        },
        "status" => {
            let running = cua_driver_daemon_running(binary).await;
            Some(Ok(AxResult {
                ok: running,
                stdout: json!({ "running": running }).to_string(),
                stderr: String::new(),
                exit_code: 0,
            }))
        },
        "doctor" => {
            let mut command = Command::new(binary);
            command
                .args(["doctor", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let result = match timeout(Duration::from_secs(30), command.output()).await {
                Ok(Ok(output)) => Ok(AxResult {
                    ok: output.status.success(),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    exit_code: output.status.code().unwrap_or(-1),
                }),
                Ok(Err(error)) => Err(format!("failed to run cua-driver doctor: {error}")),
                Err(_) => Err("cua-driver doctor timed out after 30s".to_string()),
            };
            Some(result)
        },
        "stop" => {
            let mut command = Command::new(binary);
            command
                .arg("stop")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let result = match timeout(Duration::from_secs(10), command.output()).await {
                Ok(Ok(output)) => Ok(AxResult {
                    ok: output.status.success(),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                    exit_code: output.status.code().unwrap_or(-1),
                }),
                Ok(Err(error)) => Err(format!("failed to run cua-driver stop: {error}")),
                Err(_) => Err("cua-driver stop timed out after 10s".to_string()),
            };
            Some(result)
        },
        _ => None,
    }
}

/// CuaDriver 0.28 failures that mean the element address named a snapshot
/// that is gone. `get_window_state` mints a new `snapshot_id` every time and
/// fails the old id/token closed, and a bare `element_index` is refused, so
/// refreshing and resending the SAME arguments can never succeed.
const CUA_STALE_ADDRESS_MARKERS: [&str; 3] = [
    "No cached AX state",
    "snapshot_id_required",
    "stale_element_token",
];
const CUA_STALE_ADDRESS_HINT: &str = "element address is stale: re-run get_window_state and \
     resend with its new snapshot_id/element_token";

fn cua_output_has_stale_address(output: &std::process::Output) -> bool {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    CUA_STALE_ADDRESS_MARKERS
        .iter()
        .any(|marker| stdout.contains(marker) || stderr.contains(marker))
}

/// Proxy a cua-driver action. `action` is the CLI tool name (e.g. `click`);
/// `body` is the raw JSON args object (forwarded verbatim as the `call`
/// positional). Returns the daemon's stdout/stderr/exit so the container relay
/// can re-emit cua-driver's own JSON unchanged.
/// What an AX action addressed — the window and element it names, never the
/// text it types. A one-line summary so the log says which window a click or a
/// snapshot went to without carrying a password into it.
fn ax_target_summary(args_json: &str) -> String {
    let Ok(Value::Object(args)) = serde_json::from_str::<Value>(args_json) else {
        return "-".to_string();
    };
    let mut parts = Vec::new();
    for key in [
        "pid",
        "window_id",
        "element_index",
        "element_token",
        "snapshot_id",
        "bundle_id",
        "key",
    ] {
        if let Some(value) = args.get(key) {
            let rendered = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            if !rendered.is_empty() {
                parts.push(format!("{key}={rendered}"));
            }
        }
    }
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(" ")
    }
}

async fn handle_ax(action: &str, body: &[u8]) -> Result<AxResult, String> {
    let action = action.trim().trim_matches('/');
    if action.is_empty() {
        return Err("empty ax action".to_string());
    }
    let Some(binary) = resolve_cua_driver_binary() else {
        return Err(
            "cua-driver binary not found on the host (PATH, ~/.local/bin, or MAGICIAN_CUA_DRIVER_BIN); \
             install and start CuaDriver with `make setup-cua-driver ARGS=--start`"
                .to_string(),
        );
    };

    // cua-driver's `call` CLI takes the args as a single JSON-object positional.
    // An empty body means "no args" → `{}`, matching cua_call.py's default.
    let args_json = if body.is_empty() {
        "{}".to_string()
    } else {
        let trimmed = String::from_utf8_lossy(body).trim().to_string();
        if trimmed.is_empty() {
            "{}".to_string()
        } else {
            trimmed
        }
    };

    // Daemon-lifecycle actions are top-level cua-driver subcommands, NOT
    // `call <tool>`. Mirror cua_call.py's `serve`/`status`/`stop` handling so
    // the container relay drives daemon lifecycle through the same route.
    if let Some(result) = handle_ax_lifecycle(&binary, action).await {
        return result;
    }

    // CuaDriver 0.28: every `call` goes through a daemon, including
    // check_permissions and get_config/set_config.
    if !ensure_cua_driver_daemon(&binary).await {
        return Err("CuaDriver is not running in an accessible desktop session; run CUA setup on the desktop host".into());
    }

    // Every AX action that crosses this route, named, whoever asked for it.
    // The skill's controller keeps its own ledger, but this endpoint is plain
    // HTTP to loopback: an agent holding the generic `http` tool can drive the
    // desktop here without the skill, and did — a comparison run's whole
    // desktop walk read as "no tool calls" because the only counter lived in
    // the controller. `target` names what a click or a snapshot addressed,
    // never the text an action types.
    tracing::info!(
        action = %action,
        target = %ax_target_summary(&args_json),
        args_bytes = args_json.len(),
        "host gateway AX action"
    );

    let output = run_cua_driver_call(&binary, action, &args_json).await?;
    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() && cua_output_has_stale_address(&output) {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(CUA_STALE_ADDRESS_HINT);
        stderr.push('\n');
    }

    Ok(AxResult {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr,
        exit_code: output.status.code().unwrap_or(-1),
    })
}

/// Invoke one bounded CUA operation for an authenticated Edge call while
/// sharing the same owner lock as loopback/container callers. The dispatcher
/// supplies an advertised action name and JSON object; no route or executable
/// path crosses the remote boundary.
pub(crate) async fn invoke_edge_ax(
    app: &AppHandle,
    action: &str,
    payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::to_vec(&payload)
        .map_err(|_| "Edge CUA payload could not be encoded".to_owned())?;
    let cua_owner_lock = app
        .state::<AppState>()
        .host_gateway
        .lock()
        .await
        .cua_owner_lock
        .clone();
    let _cua_owner_guard = cua_owner_lock.lock_owned().await;
    let result = handle_ax(action, &body).await?;
    serde_json::to_value(result).map_err(|_| "Edge CUA result could not be encoded".to_owned())
}

fn typed_app_macos_pairing_store_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|path| path.join("app-macos-host-pairing.json"))
        .map_err(|error| format!("typed macOS pairing store path is unavailable: {error}"))
}

pub(crate) async fn ensure_typed_app_macos_pairing_owner(app: &AppHandle) -> Result<(), String> {
    let path = typed_app_macos_pairing_store_path(app)?;
    let requirement = {
        let app_state = app.state::<AppState>();
        let mut state = app_state.host_gateway.lock().await;
        let HostGatewayState {
            app_macos,
            app_macos_identity,
            app_macos_pairing,
            ..
        } = &mut *state;
        app_macos_pairing.initialize(path, app_macos, app_macos_identity)?;
        app_macos_pairing.active_binary_requirement(app_macos)?
    };
    let Some(requirement) = requirement else {
        return Ok(());
    };
    let prevalidated = prevalidate_typed_app_macos_verifier_binary(
        requirement.clone(),
        current_unix_ms()?.saturating_add(120_000),
    )
    .await?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    app_macos_pairing.install_active_prevalidated(
        &requirement,
        prevalidated,
        app_macos,
        app_macos_identity,
    )
}

/// Begin the existing Keychain identity's one-time owner-code ceremony for the
/// Android Apps authority bootstrap. This is crate-only; the Android authority
/// module applies the trusted Settings-origin fence before calling it.
pub(crate) async fn begin_app_android_desktop_identity_approval(
    app: &AppHandle,
) -> Result<crate::app_macos_identity::AppMacosDesktopIdentityEnrollment, String> {
    ensure_typed_app_macos_pairing_owner(app).await?;
    app.state::<AppState>()
        .host_gateway
        .lock()
        .await
        .app_macos_identity
        .begin_owner_approval(current_unix_ms()?)
}

/// Produce exactly one challenge-bound Android bootstrap attestation and
/// consume the displayed owner code. No Tauri command exposes this signature
/// directly to JavaScript.
pub(crate) async fn attest_app_android_desktop_identity(
    app: &AppHandle,
    challenge: &magician_app_contract::macos_host::AppMacosDesktopIdentityChallenge,
) -> Result<magician_app_contract::macos_host::AppMacosDesktopIdentityAttestation, String> {
    ensure_typed_app_macos_pairing_owner(app).await?;
    let now_ms = current_unix_ms()?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    let host_identity_digest = app_macos_pairing.ensure_host_identity_digest()?;
    let attestation = app_macos_identity.attest(challenge, host_identity_digest, now_ms)?;
    app_macos_identity.consume_owner_approval(challenge, now_ms)?;
    Ok(attestation)
}

async fn prevalidate_typed_app_macos_verifier_binary(
    requirement: crate::app_macos_pairing::AppMacosVerifierBinaryRequirement,
    expires_at_ms: i64,
) -> Result<crate::app_macos_host::AppMacosPrevalidatedCuaBinary, String> {
    let stop = crate::app_macos_host::AppMacosHostStopSignal::new();
    let slot = reserve_typed_app_macos_identity_hash_slot(&stop, expires_at_ms).await?;
    let slot = Arc::new(slot);
    let blocking_slot = Arc::clone(&slot);
    let binary = requirement.binary().to_path_buf();
    let expected_digest = requirement.binary_digest().to_owned();
    let mut hashing = tokio::task::spawn_blocking(move || {
        let _identity_hash_slot = blocking_slot;
        crate::app_macos_host::app_macos_host_prevalidate_binary(&binary, &expected_digest)
    });
    let remaining_ms = expires_at_ms
        .checked_sub(current_unix_ms()?)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "typed macOS verifier prevalidation expired".to_owned())?;
    let deadline = tokio::time::sleep(Duration::from_millis(
        u64::try_from(remaining_ms)
            .map_err(|_| "typed macOS verifier prevalidation lifetime is invalid".to_owned())?,
    ));
    tokio::pin!(deadline);
    tokio::select! {
        result = &mut hashing => result
            .map_err(|_| "typed macOS verifier prevalidation was interrupted".to_owned())?
            .ok_or_else(|| "typed macOS verifier binary identity is stale".to_owned()),
        _ = stop.stopped() => Err("typed macOS verifier prevalidation was cancelled".to_owned()),
        _ = &mut deadline => Err("typed macOS verifier prevalidation exceeded its deadline".to_owned()),
    }
}

async fn handle_typed_app_macos_pairing_proposal(
    app: &AppHandle,
    body: &[u8],
) -> Result<Value, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("typed macOS pairing proposal is empty or oversized".to_owned());
    }
    ensure_typed_app_macos_pairing_owner(app).await?;
    let proposal: magician_app_contract::macos_host::AppMacosHostPairingProposal =
        serde_json::from_slice(body)
            .map_err(|_| "typed macOS pairing proposal is malformed".to_owned())?;
    let now_ms = current_unix_ms()?;
    let setup_id = proposal.setup_id.clone();
    let generation = proposal.generation;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_pairing,
        ..
    } = &mut *state;
    app_macos_pairing.receive_proposal(proposal, now_ms, app_macos)?;
    Ok(json!({
        "schema": magician_app_contract::macos_host::APP_MACOS_HOST_PAIRING_V1,
        "status": "pending_native_approval",
        "setup_id": setup_id,
        "generation": generation,
    }))
}

async fn handle_typed_app_macos_identity_attestation(
    app: &AppHandle,
    body: &[u8],
) -> Result<magician_app_contract::macos_host::AppMacosDesktopIdentityAttestation, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("typed macOS desktop identity challenge is empty or oversized".to_owned());
    }
    ensure_typed_app_macos_pairing_owner(app).await?;
    let challenge: magician_app_contract::macos_host::AppMacosDesktopIdentityChallenge =
        serde_json::from_slice(body)
            .map_err(|_| "typed macOS desktop identity challenge is malformed".to_owned())?;
    let now_ms = current_unix_ms()?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    if let Some(attestation) = state
        .app_macos_pairing
        .retained_identity_attestation(&challenge, now_ms)?
    {
        return Ok(attestation);
    }
    let host_identity_digest = state.app_macos_pairing.ensure_host_identity_digest()?;
    let attestation = state
        .app_macos_identity
        .attest(&challenge, host_identity_digest, now_ms)?;
    state.app_macos_pairing.record_identity_attestation(
        challenge.clone(),
        attestation.clone(),
        now_ms,
    )?;
    state
        .app_macos_identity
        .consume_owner_approval(&challenge, now_ms)?;
    Ok(attestation)
}

async fn handle_typed_app_macos_pairing_status(
    app: &AppHandle,
    body: &[u8],
) -> Result<magician_app_contract::macos_host::AppMacosHostPairingStatusResponse, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("typed macOS pairing status request is empty or oversized".to_owned());
    }
    ensure_typed_app_macos_pairing_owner(app).await?;
    let request: magician_app_contract::macos_host::AppMacosHostPairingStatusRequest =
        serde_json::from_slice(body)
            .map_err(|_| "typed macOS pairing status request is malformed".to_owned())?;
    app.state::<AppState>()
        .host_gateway
        .lock()
        .await
        .app_macos_pairing
        .status(&request, current_unix_ms()?)
}

async fn handle_typed_app_macos_pairing_revoke(
    app: &AppHandle,
    body: &[u8],
) -> Result<magician_app_contract::macos_host::AppMacosHostPairingStatusResponse, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("typed macOS pairing revoke request is empty or oversized".to_owned());
    }
    ensure_typed_app_macos_pairing_owner(app).await?;
    let request: magician_app_contract::macos_host::AppMacosHostPairingStatusRequest =
        serde_json::from_slice(body)
            .map_err(|_| "typed macOS pairing revoke request is malformed".to_owned())?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    let revoked = app_macos_pairing.revoke_from_runtime(
        &request,
        current_unix_ms()?,
        app_macos,
        app_macos_identity,
    )?;
    Ok(magician_app_contract::macos_host::AppMacosHostPairingStatusResponse::Revoked { revoked })
}

async fn handle_typed_app_macos_pairing_finalization(
    app: &AppHandle,
    body: &[u8],
) -> Result<magician_app_contract::macos_host::AppMacosHostPairingFinalized, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("typed macOS pairing finalization is empty or oversized".to_owned());
    }
    ensure_typed_app_macos_pairing_owner(app).await?;
    let finalization: magician_app_contract::macos_host::AppMacosHostPairingFinalization =
        serde_json::from_slice(body)
            .map_err(|_| "typed macOS pairing finalization is malformed".to_owned())?;
    let requirement = app
        .state::<AppState>()
        .host_gateway
        .lock()
        .await
        .app_macos_pairing
        .pending_binary_requirement(&finalization)?;
    let prevalidated = prevalidate_typed_app_macos_verifier_binary(
        requirement.clone(),
        current_unix_ms()?.saturating_add(120_000),
    )
    .await?;
    let app_state = app.state::<AppState>();
    let mut state = app_state.host_gateway.lock().await;
    let HostGatewayState {
        app_macos,
        app_macos_identity,
        app_macos_pairing,
        ..
    } = &mut *state;
    app_macos_pairing.finalize(
        finalization,
        current_unix_ms()?,
        app_macos,
        app_macos_identity,
        &requirement,
        prevalidated,
    )
}

/// Record-free Android authority liveness/high-water response. This endpoint
/// deliberately cannot stage, approve, recover, revoke, or disclose any
/// target record; those operations exist only on the bundled Settings IPC
/// surface.
async fn handle_typed_app_android_owner_status(
    app: &AppHandle,
    body: &[u8],
) -> Result<magician_app_contract::android_owner::AppAndroidOwnerStatusReceipt, String> {
    if body.is_empty() || body.len() > crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES {
        return Err("Android owner status challenge is empty or oversized".to_owned());
    }
    let challenge: magician_app_contract::android_owner::AppAndroidOwnerStatusChallenge =
        serde_json::from_slice(body)
            .map_err(|_| "Android owner status challenge is malformed".to_owned())?;
    crate::app_android_authority::signed_status_for_gateway(app, challenge).await
}

fn current_unix_ms() -> Result<i64, String> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    i64::try_from(value).map_err(|_| "system clock is out of range".to_owned())
}

/// Typed Apps-only macOS route. The desktop verifier consumes the signed
/// nonce before any CUA poll, then this final fence rechecks current TCC,
/// process/bundle and window identity. The legacy raw AX route is never called
/// with an app-provided action name or JSON object.
async fn handle_typed_app_macos_action(app: &AppHandle, body: &[u8]) -> Result<Value, String> {
    init_typed_app_macos_daemon_home(app)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    let now_ms = i64::try_from(now_ms).map_err(|_| "system clock is out of range".to_owned())?;
    let authorized = {
        let app_state = app.state::<AppState>();
        let mut state = app_state.host_gateway.lock().await;
        state
            .app_macos
            .authorize(body, now_ms)
            .map_err(|error| error.to_string())?
    };
    let correlation_ref = authorized.correlation_ref().to_owned();
    let lowered = match authorized.lower() {
        Ok(lowered) => lowered,
        Err(error) => {
            app.state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .finish_action(&correlation_ref);
            return Err(error.to_string());
        },
    };
    let stop_signal = lowered.stop_signal();
    // Reserve bounded blocking-file capacity before the CUA owner lock. The
    // verifier permits several pending actions, but only this small pool may
    // hash the 128-MiB owner binary and 512-MiB app executable. Acquisition is
    // stop/expiry-aware and never queues while holding CUA or workflow state.
    let identity_hash_slot =
        match reserve_typed_app_macos_identity_hash_slot(&stop_signal, lowered.expires_at_ms())
            .await
        {
            Ok(slot) => slot,
            Err(error) => {
                app.state::<AppState>()
                    .host_gateway
                    .lock()
                    .await
                    .app_macos
                    .finish_action(&correlation_ref);
                return Err(error);
            },
        };
    let cua_owner_lock = app
        .state::<AppState>()
        .host_gateway
        .lock()
        .await
        .cua_owner_lock
        .clone();
    let _cua_owner_guard = tokio::select! {
        guard = cua_owner_lock.lock_owned() => guard,
        _ = stop_signal.stopped() => {
            app.state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .finish_action(&correlation_ref);
            return Err("typed macOS action was stopped before physical owner I/O".to_owned());
        },
    };
    let current_ms = match SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
    {
        Some(current_ms) => current_ms,
        None => {
            app.state::<AppState>()
                .host_gateway
                .lock()
                .await
                .app_macos
                .finish_action(&correlation_ref);
            return Err("system clock is outside the typed macOS range".to_owned());
        },
    };
    if current_ms >= lowered.expires_at_ms() {
        app.state::<AppState>()
            .host_gateway
            .lock()
            .await
            .app_macos
            .finish_action(&correlation_ref);
        return Err("typed macOS permit expired while waiting for the physical owner".to_owned());
    }
    let execution_deadline = tokio::time::sleep(Duration::from_millis(
        u64::try_from(lowered.expires_at_ms().saturating_sub(current_ms))
            .map_err(|_| "typed macOS permit lifetime is out of range".to_owned())?,
    ));
    tokio::pin!(execution_deadline);
    let result = tokio::select! {
        result = execute_typed_app_macos_action(lowered, identity_hash_slot) => result,
        _ = stop_signal.stopped() => {
            Err("typed macOS action was stopped during physical owner I/O".to_owned())
        },
        _ = &mut execution_deadline => {
            Err("typed macOS action exceeded its physical owner permit deadline".to_owned())
        },
    };
    app.state::<AppState>()
        .host_gateway
        .lock()
        .await
        .app_macos
        .finish_action(&correlation_ref);
    result
}

fn typed_app_macos_identity_hash_slots() -> &'static Arc<Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(Semaphore::new(APP_MACOS_TYPED_IDENTITY_HASH_SLOTS)))
}

async fn reserve_typed_app_macos_identity_hash_slot(
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<OwnedSemaphorePermit, String> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .ok_or_else(|| "system clock is outside the typed macOS range".to_owned())?;
    let remaining_ms = expires_at_ms
        .checked_sub(now_ms)
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "typed macOS permit expired before identity hashing".to_owned())?;
    let deadline = tokio::time::sleep(Duration::from_millis(
        u64::try_from(remaining_ms)
            .map_err(|_| "typed macOS permit lifetime is out of range".to_owned())?,
    ));
    tokio::pin!(deadline);
    tokio::select! {
        permit = Arc::clone(typed_app_macos_identity_hash_slots()).acquire_owned() => {
            permit.map_err(|_| "typed macOS identity hash capacity is unavailable".to_owned())
        },
        _ = stop_signal.stopped() => {
            Err("typed macOS identity hash was cancelled before admission".to_owned())
        },
        _ = &mut deadline => {
            Err("typed macOS identity hash admission exceeded its permit deadline".to_owned())
        },
    }
}

async fn execute_typed_app_macos_action(
    mut lowered: crate::app_macos_host::AppMacosLoweredCuaCall,
    identity_hash_slot: OwnedSemaphorePermit,
) -> Result<Value, String> {
    let binary = lowered.cua_driver_binary().to_path_buf();
    let owner_stop = lowered.stop_signal();
    let digest_path = binary.clone();
    let identity_hash_slot = Arc::new(identity_hash_slot);
    let initial_identity_slot = Arc::clone(&identity_hash_slot);
    let bundle_id = lowered.bundle_id().to_owned();
    let process_id = lowered.process_id();
    let (current_binary_digest, current_application_identity) =
        tokio::task::spawn_blocking(move || {
            let _identity_slot = initial_identity_slot;
            (
                crate::app_macos_host::app_macos_host_binary_digest(&digest_path),
                crate::app_macos_host::app_macos_host_current_application_identity_digest_blocking(
                    &bundle_id, process_id,
                ),
            )
        })
        .await
        .map_err(|_| "typed macOS binary identity check was interrupted".to_owned())?;
    if current_binary_digest.as_deref() != Some(lowered.cua_driver_binary_digest()) {
        return Err("typed macOS physical owner binary identity changed before I/O".to_owned());
    }
    let current_application_identity = current_application_identity
        .ok_or_else(|| "typed macOS application identity could not be re-observed".to_owned())?;
    if current_application_identity != lowered.application_identity_digest() {
        return Err("typed macOS application identity changed before I/O".to_owned());
    }
    let permissions = handle_typed_app_macos_ax(
        &binary,
        lowered.cua_driver_binary_digest(),
        "check_permissions",
        APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS,
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        &owner_stop,
        lowered.expires_at_ms(),
    )
    .await?;
    let current_tcc_policy = typed_app_macos_tcc_policy_observation(&permissions.stdout)?;
    if !permissions.ok
        || !current_tcc_policy.accessibility
        || (lowered.requires_screen_recording() && !current_tcc_policy.screen_recording)
    {
        return Err("current macOS Accessibility/Screen Recording grant is unavailable".to_owned());
    }
    if current_tcc_policy.digest != lowered.tcc_policy_digest() {
        return Err("current macOS TCC policy changed after owner approval".to_owned());
    }
    if lowered.dynamic_observation_target() {
        let (process_id, window_id) = typed_app_macos_resolve_sole_observation_target(
            &binary,
            lowered.cua_driver_binary_digest(),
            lowered.bundle_id(),
            &owner_stop,
            lowered.expires_at_ms(),
        )
        .await?;
        let running_identity_slot = Arc::clone(&identity_hash_slot);
        let running_bundle_id = lowered.bundle_id().to_owned();
        let running_application_identity = tokio::task::spawn_blocking(move || {
            let _identity_slot = running_identity_slot;
            crate::app_macos_host::app_macos_host_current_application_identity_digest_blocking(
                &running_bundle_id,
                Some(process_id),
            )
        })
        .await
        .map_err(|_| "typed macOS running application identity check was interrupted".to_owned())?
        .ok_or_else(|| {
            "typed macOS running application identity could not be re-observed".to_owned()
        })?;
        if running_application_identity != lowered.application_identity_digest() {
            return Err(
                "typed macOS running application identity changed before observation".to_owned(),
            );
        }
        lowered
            .bind_dynamic_observation_target(process_id, window_id)
            .map_err(|error| error.to_string())?;
    }
    if let Some(process_id) = lowered.process_id() {
        typed_app_macos_revalidate_process(
            &binary,
            lowered.cua_driver_binary_digest(),
            lowered.bundle_id(),
            process_id,
            &owner_stop,
            lowered.expires_at_ms(),
        )
        .await?;
    }
    if let (Some(process_id), Some(window_id)) = (lowered.process_id(), lowered.window_id()) {
        typed_app_macos_revalidate_window(
            &binary,
            lowered.cua_driver_binary_digest(),
            process_id,
            window_id,
            &owner_stop,
            lowered.expires_at_ms(),
        )
        .await?;
    }
    if let Some(expected_content_digest) = lowered.observation_content_digest() {
        let revalidation_byte_ceiling = lowered
            .observation_revalidation_byte_ceiling()
            .ok_or_else(|| "typed macOS observation fence lacks a byte ceiling".to_owned())?;
        let process_id = lowered
            .process_id()
            .ok_or_else(|| "typed macOS observation fence lacks a process".to_owned())?;
        let window_id = lowered
            .window_id()
            .ok_or_else(|| "typed macOS observation fence lacks a window".to_owned())?;
        let fresh_observation = typed_app_macos_revalidate_observation(
            &binary,
            lowered.cua_driver_binary_digest(),
            process_id,
            window_id,
            expected_content_digest,
            &lowered.observation_fence_indexes(),
            revalidation_byte_ceiling,
            &owner_stop,
            lowered.expires_at_ms(),
        )
        .await?;
        // The fence minted a new snapshot and superseded the runtime's token;
        // the unchanged element fence proves its index still names the same row.
        lowered
            .bind_fresh_observation(&fresh_observation)
            .map_err(|_| "typed macOS element is absent from the fenced snapshot".to_owned())?;
    }
    if lowered.requires_fresh_element_binding() {
        return Err("typed macOS element action lacks a fenced fresh snapshot".to_owned());
    }

    // Discovery, window and observation preflights above are all awaited and
    // therefore are not an identity fence for the final physical read. Check
    // the exact paired policy and the exact running executable again here. A
    // post-read fence below prevents returning bytes if either identity drifts
    // while the reviewed Observe verb is in flight.
    let final_process_id = lowered
        .process_id()
        .ok_or_else(|| "typed macOS observation lacks a final running process".to_owned())?;
    typed_app_macos_observe_owner_fence(
        &binary,
        lowered.cua_driver_binary_digest(),
        lowered.bundle_id(),
        final_process_id,
        lowered.application_identity_digest(),
        lowered.tcc_policy_digest(),
        lowered.requires_screen_recording(),
        &identity_hash_slot,
        &owner_stop,
        lowered.expires_at_ms(),
        "immediately before physical owner I/O",
    )
    .await?;

    let args = lowered.args_bytes().map_err(|error| error.to_string())?;
    let result = handle_typed_app_macos_ax(
        &binary,
        lowered.cua_driver_binary_digest(),
        lowered.verb(),
        &args,
        lowered.response_byte_ceiling(),
        &owner_stop,
        lowered.expires_at_ms(),
    )
    .await?;
    let result_bytes = result
        .stdout
        .len()
        .checked_add(result.stderr.len())
        .ok_or_else(|| "typed macOS result length overflow".to_owned())?;
    if result_bytes > lowered.response_byte_ceiling() {
        return Err("typed macOS result exceeds its reviewed byte ceiling".to_owned());
    }
    let owner_payload_bytes = u64::try_from(result.stdout.len())
        .map_err(|_| "typed macOS owner payload length is out of range".to_owned())?;
    let owner_payload_ceiling = if lowered.mutation() {
        lowered.result_byte_ceiling()
    } else {
        lowered.evidence_byte_ceiling()
    };
    if owner_payload_bytes > owner_payload_ceiling {
        return Err("typed macOS owner payload exceeds its specific reviewed ceiling".to_owned());
    }
    if !result.ok {
        return Err("typed macOS owner reported an unsuccessful action".to_owned());
    }
    if !lowered.mutation() {
        typed_app_macos_observe_owner_fence(
            &binary,
            lowered.cua_driver_binary_digest(),
            lowered.bundle_id(),
            final_process_id,
            lowered.application_identity_digest(),
            lowered.tcc_policy_digest(),
            lowered.requires_screen_recording(),
            &identity_hash_slot,
            &owner_stop,
            lowered.expires_at_ms(),
            "during physical owner I/O",
        )
        .await?;
    }
    if lowered.verb() == "launch_app" {
        let post_launch_identity_slot = Arc::clone(&identity_hash_slot);
        let bundle_id = lowered.bundle_id().to_owned();
        let post_launch_identity = tokio::task::spawn_blocking(move || {
            let _identity_slot = post_launch_identity_slot;
            crate::app_macos_host::app_macos_host_current_application_identity_digest_blocking(
                &bundle_id, None,
            )
        })
        .await
        .map_err(|_| "typed macOS launched application identity check was interrupted".to_owned())?
        .ok_or_else(|| {
            "typed macOS launched application identity could not be re-observed".to_owned()
        })?;
        if post_launch_identity != lowered.application_identity_digest() {
            return Err("typed macOS launched application identity changed during I/O".to_owned());
        }
    }

    if lowered.mutation() {
        typed_app_macos_bounded_response(
            &lowered,
            json!({
                "schema": "magician.app-macos-host-result.v1",
                "correlation_ref": lowered.correlation_ref(),
                "effect_binding_digest": lowered.effect_binding_digest(),
                "tcc_policy_digest": lowered.tcc_policy_digest(),
                "tcc_epoch": lowered.tcc_epoch(),
                "outcome": "completed",
            }),
        )
    } else {
        if magician_app_contract::macos_host::app_macos_host_observation_contains_secure_content(
            &result.stdout,
        ) {
            return Err("typed macOS observation contains a protected or secure target".to_owned());
        }
        typed_app_macos_bounded_response(
            &lowered,
            json!({
                "schema": "magician.app-macos-host-result.v1",
                "correlation_ref": lowered.correlation_ref(),
                "effect_binding_digest": lowered.effect_binding_digest(),
                "tcc_policy_digest": lowered.tcc_policy_digest(),
                "tcc_epoch": lowered.tcc_epoch(),
                "outcome": "completed",
                "observation": result.stdout,
                "process_id": lowered.process_id(),
                "window_id": lowered.window_id(),
            }),
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn typed_app_macos_observe_owner_fence(
    binary: &PathBuf,
    binary_digest: &str,
    bundle_id: &str,
    process_id: u32,
    expected_application_identity_digest: &str,
    expected_tcc_policy_digest: &str,
    requires_screen_recording: bool,
    identity_hash_slot: &Arc<OwnedSemaphorePermit>,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
    phase: &'static str,
) -> Result<(), String> {
    let permissions = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "check_permissions",
        APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS,
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    let current_tcc_policy = typed_app_macos_tcc_policy_observation(&permissions.stdout)?;

    let retained_identity_hash_slot = Arc::clone(identity_hash_slot);
    let bundle_id = bundle_id.to_owned();
    let current_application_identity = tokio::task::spawn_blocking(move || {
        let _identity_hash_slot = retained_identity_hash_slot;
        crate::app_macos_host::app_macos_host_current_application_identity_digest_blocking(
            &bundle_id,
            Some(process_id),
        )
    })
    .await
    .map_err(|_| format!("typed macOS application identity check was interrupted {phase}"))?
    .ok_or_else(|| format!("typed macOS application identity could not be re-observed {phase}"))?;

    if !typed_app_macos_owner_fence_matches(
        permissions.ok,
        &current_tcc_policy,
        requires_screen_recording,
        &current_application_identity,
        expected_tcc_policy_digest,
        expected_application_identity_digest,
    ) {
        return Err(format!(
            "typed macOS TCC or application identity changed {phase}"
        ));
    }
    Ok(())
}

fn typed_app_macos_owner_fence_matches(
    permissions_call_ok: bool,
    current_tcc_policy: &AppMacosTccPolicyObservation,
    requires_screen_recording: bool,
    current_application_identity_digest: &str,
    expected_tcc_policy_digest: &str,
    expected_application_identity_digest: &str,
) -> bool {
    permissions_call_ok
        && current_tcc_policy.accessibility
        && (!requires_screen_recording || current_tcc_policy.screen_recording)
        && current_tcc_policy.digest == expected_tcc_policy_digest
        && current_application_identity_digest == expected_application_identity_digest
}

async fn typed_app_macos_resolve_sole_observation_target(
    binary: &PathBuf,
    binary_digest: &str,
    bundle_id: &str,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<(u32, u32), String> {
    let applications = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "list_apps",
        b"{}",
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    if !applications.ok {
        return Err("typed macOS application discovery failed".to_owned());
    }
    let process_id = typed_app_macos_unique_process(&applications.stdout, bundle_id)
        .ok_or_else(|| "typed macOS target must have exactly one running process".to_owned())?;
    let windows_args = serde_json::to_vec(&json!({ "pid": process_id }))
        .map_err(|_| "typed macOS window discovery could not be encoded".to_owned())?;
    let windows = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "list_windows",
        &windows_args,
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    if !windows.ok {
        return Err("typed macOS window discovery failed".to_owned());
    }
    let window_id = typed_app_macos_unique_window(&windows.stdout)
        .ok_or_else(|| "typed macOS target must have exactly one current window".to_owned())?;
    Ok((process_id, window_id))
}

fn typed_app_macos_bounded_response(
    lowered: &crate::app_macos_host::AppMacosLoweredCuaCall,
    response: Value,
) -> Result<Value, String> {
    let encoded = serde_json::to_vec(&response)
        .map_err(|_| "failed to encode typed macOS result".to_owned())?;
    if encoded.len() > lowered.response_byte_ceiling() {
        return Err("typed macOS encoded result exceeds its reviewed byte ceiling".to_owned());
    }
    Ok(response)
}

/// Fixed-binary CUA call used only by the typed Apps owner. Unlike the legacy
/// AX route, this never consults environment variables, PATH, HOME or fallback
/// application locations, never talks to CuaDriver.app's shared daemon, never
/// starts a daemon through a generic launcher, and never retries a possibly
/// effectful operation. Every call goes to the owner's private embedded
/// daemon, started lazily from the same approved binary digest.
async fn handle_typed_app_macos_ax(
    binary: &PathBuf,
    binary_digest: &str,
    action: &'static str,
    body: &[u8],
    output_byte_ceiling: usize,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<AxResult, String> {
    if !matches!(
        action,
        "check_permissions"
            | "list_apps"
            | "list_windows"
            | "launch_app"
            | "bring_to_front"
            | "get_window_state"
            | "click"
            | "double_click"
            | "type_text"
            | "press_key"
            | "scroll"
            | "drag"
    ) {
        return Err("typed macOS action is not in the reviewed CUA verb set".to_owned());
    }
    let args_json = std::str::from_utf8(body)
        .map_err(|_| "typed macOS CUA arguments are not UTF-8".to_owned())?;
    let value: Value = serde_json::from_str(args_json)
        .map_err(|_| "typed macOS CUA arguments are malformed".to_owned())?;
    if !value.is_object() {
        return Err("typed macOS CUA arguments must be an object".to_owned());
    }
    let socket =
        ensure_typed_app_macos_private_daemon(binary, binary_digest, stop_signal, expires_at_ms)
            .await?;
    let output = run_typed_cua_driver_call(
        binary,
        binary_digest,
        &socket,
        action,
        args_json,
        output_byte_ceiling,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| "typed macOS owner returned non-UTF-8 stdout".to_owned())?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|_| "typed macOS owner returned non-UTF-8 stderr".to_owned())?;
    Ok(AxResult {
        ok: output.status.success(),
        stdout,
        stderr,
        exit_code: output.status.code().unwrap_or(-1),
    })
}

async fn typed_app_macos_revalidate_process(
    binary: &PathBuf,
    binary_digest: &str,
    bundle_id: &str,
    process_id: u32,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<(), String> {
    let result = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "list_apps",
        b"{}",
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    if !result.ok || !typed_app_macos_json_has_process(&result.stdout, bundle_id, process_id) {
        return Err("typed macOS application identity changed before I/O".to_owned());
    }
    Ok(())
}

async fn typed_app_macos_revalidate_window(
    binary: &PathBuf,
    binary_digest: &str,
    process_id: u32,
    window_id: u32,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<(), String> {
    let args = serde_json::to_vec(&json!({ "pid": process_id }))
        .map_err(|error| format!("failed to encode typed window fence: {error}"))?;
    let result = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "list_windows",
        &args,
        APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    if !result.ok || !typed_app_macos_json_has_window(&result.stdout, window_id) {
        return Err("typed macOS window identity changed before I/O".to_owned());
    }
    Ok(())
}

/// Re-snapshot the window and require the fence the runtime derived from its
/// observation: for an element action, the target's own `tree_markdown` line
/// and its ancestors' indexes and roles (both drag endpoints); for a key
/// press, the window row and its children's roles
/// (`app_macos_host_observation_fence_digest`). Not the whole window: macOS
/// retitles a document on its own after an edit, and the title is an ancestor
/// of every element. The tree only (`include_screenshot:false`): a fresh
/// screenshot is not part of the digest and could exceed the ceiling derived
/// from the original evidence. Returns the fresh reply, whose `snapshot_id`
/// and `elements[]` are the only addresses CuaDriver 0.28 still accepts.
#[allow(clippy::too_many_arguments)]
async fn typed_app_macos_revalidate_observation(
    binary: &PathBuf,
    binary_digest: &str,
    process_id: u32,
    window_id: u32,
    expected_content_digest: &str,
    fence_indexes: &[u32],
    evidence_byte_ceiling: u64,
    stop_signal: &crate::app_macos_host::AppMacosHostStopSignal,
    expires_at_ms: i64,
) -> Result<Value, String> {
    let args = serde_json::to_vec(&json!({
        "pid": process_id,
        "window_id": window_id,
        "include_screenshot": false,
    }))
    .map_err(|error| format!("failed to encode typed observation fence: {error}"))?;
    let output_byte_ceiling = usize::try_from(evidence_byte_ceiling)
        .map_err(|_| "typed macOS observation ceiling is out of range".to_owned())?;
    let result = handle_typed_app_macos_ax(
        binary,
        binary_digest,
        "get_window_state",
        &args,
        output_byte_ceiling,
        stop_signal,
        expires_at_ms,
    )
    .await?;
    // Three different failures, reported apart: one message for all of them
    // left a failed drag fence undiagnosable.
    if !result.ok {
        let stderr: String = result.stderr.trim().chars().take(240).collect();
        return Err(format!(
            "typed macOS observation fence failed (exit {}): {stderr}",
            result.exit_code
        ));
    }
    if u64::try_from(result.stdout.len()).unwrap_or(u64::MAX) > evidence_byte_ceiling {
        return Err("typed macOS observation fence exceeds its byte ceiling".to_owned());
    }
    if magician_app_contract::macos_host::app_macos_host_observation_contains_secure_content(
        &result.stdout,
    ) {
        return Err(
            "typed macOS observation fence contains a protected or secure target".to_owned(),
        );
    }
    let value: Value = serde_json::from_str(&result.stdout)
        .map_err(|_| "typed macOS refreshed observation is malformed".to_owned())?;
    let tree = value
        .get("tree_markdown")
        .and_then(Value::as_str)
        .ok_or_else(|| "typed macOS refreshed observation lacks a tree".to_owned())?;
    let current_fence_digest =
        magician_app_contract::macos_host::app_macos_host_observation_fence_digest(
            tree,
            fence_indexes,
        )
        .ok_or_else(|| {
            "typed macOS action target is absent, duplicated or menu chrome in the fenced snapshot"
                .to_owned()
        })?;
    if current_fence_digest != expected_content_digest {
        return Err("typed macOS observation changed before physical action".to_owned());
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppMacosTccPolicyObservation {
    accessibility: bool,
    screen_recording: bool,
    digest: String,
}

fn typed_app_macos_tcc_policy_observation(
    stdout: &str,
) -> Result<AppMacosTccPolicyObservation, String> {
    let value: Value = serde_json::from_str(stdout)
        .map_err(|_| "typed macOS TCC observation is malformed".to_owned())?;
    let accessibility = typed_app_macos_permission_value(
        &value,
        &["accessibility", "accessibilitypermission", "axtrusted"],
    );
    let screen_recording = typed_app_macos_permission_value(
        &value,
        &[
            "screenrecording",
            "screencapture",
            "screenrecordingpermission",
        ],
    );
    let canonical = serde_json::to_vec(&json!({
        "schema": "magician.app-macos-host-tcc-observation.v1",
        "accessibility": accessibility,
        "screen_recording": screen_recording,
    }))
    .map_err(|_| "typed macOS TCC observation could not be encoded".to_owned())?;
    Ok(AppMacosTccPolicyObservation {
        accessibility,
        screen_recording,
        digest: format!("blake3:{}", blake3::hash(&canonical).to_hex()),
    })
}

fn typed_app_macos_permission_value(value: &Value, names: &[&str]) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                for (key, value) in values {
                    let normalized = key
                        .bytes()
                        .filter(|byte| byte.is_ascii_alphanumeric())
                        .map(|byte| byte.to_ascii_lowercase())
                        .collect::<Vec<_>>();
                    if names.iter().any(|name| normalized == name.as_bytes())
                        && typed_app_macos_permission_node_is_granted(value)
                    {
                        return true;
                    }
                    pending.push(value);
                }
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    false
}

fn typed_app_macos_permission_node_is_granted(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "granted" | "authorized" | "trusted"
        ),
        Value::Object(values) => ["granted", "authorized", "trusted", "allowed"]
            .iter()
            .any(|key| values.get(*key).and_then(Value::as_bool) == Some(true)),
        Value::Null | Value::Number(_) | Value::Array(_) => false,
    }
}

fn typed_app_macos_json_has_process(stdout: &str, bundle_id: &str, process_id: u32) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return false;
    };
    typed_app_macos_find_object(&value, |values| {
        let bundle_matches = ["bundle_id", "bundleId", "bundle_identifier"]
            .iter()
            .any(|key| values.get(*key).and_then(Value::as_str) == Some(bundle_id));
        let process_matches = ["pid", "process_id", "processId"]
            .iter()
            .any(|key| values.get(*key).and_then(Value::as_u64) == Some(u64::from(process_id)));
        bundle_matches && process_matches
    })
}

fn typed_app_macos_json_has_window(stdout: &str, window_id: u32) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return false;
    };
    typed_app_macos_find_object(&value, |values| {
        ["window_id", "windowId", "id"]
            .iter()
            .any(|key| values.get(*key).and_then(Value::as_u64) == Some(u64::from(window_id)))
    })
}

fn typed_app_macos_unique_process(stdout: &str, bundle_id: &str) -> Option<u32> {
    let value = serde_json::from_str::<Value>(stdout).ok()?;
    let mut process_ids = BTreeSet::new();
    let mut pending = vec![&value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                let bundle_matches = ["bundle_id", "bundleId", "bundle_identifier"]
                    .iter()
                    .any(|key| values.get(*key).and_then(Value::as_str) == Some(bundle_id));
                if bundle_matches {
                    if let Some(process_id) = ["pid", "process_id", "processId"]
                        .iter()
                        .find_map(|key| values.get(*key).and_then(Value::as_u64))
                        .and_then(|value| u32::try_from(value).ok())
                        .filter(|value| *value != 0)
                    {
                        process_ids.insert(process_id);
                    }
                }
                pending.extend(values.values());
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    (process_ids.len() == 1)
        .then(|| process_ids.into_iter().next())
        .flatten()
}

fn typed_app_macos_unique_window(stdout: &str) -> Option<u32> {
    let value = serde_json::from_str::<Value>(stdout).ok()?;
    let mut window_ids = BTreeSet::new();
    let mut pending = vec![&value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                let explicit_window_id = ["window_id", "windowId"]
                    .iter()
                    .find_map(|key| values.get(*key).and_then(Value::as_u64))
                    .or_else(|| {
                        let looks_like_window = ["title", "name", "bounds", "frame", "pid"]
                            .iter()
                            .any(|key| values.contains_key(*key));
                        looks_like_window
                            .then(|| values.get("id").and_then(Value::as_u64))
                            .flatten()
                    });
                if let Some(window_id) = explicit_window_id
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value != 0)
                {
                    window_ids.insert(window_id);
                }
                pending.extend(values.values());
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    (window_ids.len() == 1)
        .then(|| window_ids.into_iter().next())
        .flatten()
}

fn typed_app_macos_find_object(
    value: &Value,
    predicate: impl Fn(&serde_json::Map<String, Value>) -> bool,
) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Array(values) => pending.extend(values),
            Value::Object(values) => {
                if predicate(values) {
                    return true;
                }
                pending.extend(values.values());
            },
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
        }
    }
    false
}

fn temp_automation_path(ext: &str) -> Result<PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push("magician-macos-automation");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create automation temp dir: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(dir.join(format!("screen-{}-{stamp}.{ext}", std::process::id())))
}

async fn handle_overlay_draw(app: &AppHandle, body: &[u8]) -> Result<(), String> {
    let shape: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| format!("invalid draw payload: {error}"))?;
    crate::overlay::emit_overlay_draw_shape(app, shape).await
}

async fn handle_contextual_assist_open(
    app: &AppHandle,
    body: &[u8],
) -> Result<serde_json::Value, String> {
    let request: ContextualAssistOpenRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid contextual-assist payload: {error}"))?;
    let config = cloned_config(app).await;
    if !config.contextual_assist.enabled {
        return Ok(json!({ "ok": false, "reason": "disabled" }));
    }
    if request
        .app
        .as_deref()
        .map(|app_name| {
            config
                .contextual_assist
                .excluded_apps
                .iter()
                .any(|excluded| excluded.eq_ignore_ascii_case(app_name))
        })
        .unwrap_or(false)
    {
        return Ok(json!({ "ok": false, "reason": "excluded_app" }));
    }

    let Some(state) = contextual_assist_state_from_request(&request) else {
        return Ok(json!({ "ok": false, "reason": "ineligible" }));
    };
    let (anchor_x, anchor_y) = contextual_assist_anchor_from_request(app, &request)?;
    let context_text = request
        .context_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned);
    let target = crate::contextual_assist::NativeAssistTarget {
        state: state.to_string(),
        app: request.app.clone(),
        window_title: request.window_title.clone(),
        url: request.url.clone(),
        context_text,
        source: request.source.clone(),
        frame_url: request.frame_url.clone(),
        browser_tab_id: request.tab_id,
        browser_window_id: request.window_id,
        window_rect: request.window_rect.clone(),
    };
    let has_context_text = target.context_text.is_some();
    crate::overlay::show_contextual_assist_menu_centered_at_point(app, anchor_x, anchor_y, target)?;
    Ok(json!({
        "ok": true,
        "source": request.source.as_deref().unwrap_or("host_gateway"),
        "state": state,
        "has_context_text": has_context_text,
    }))
}

fn contextual_assist_state_from_request(request: &ContextualAssistOpenRequest) -> Option<&str> {
    match request.state.as_deref() {
        Some("selection" | "selection-field" | "draft" | "empty-context" | "page-context") => {
            return request.state.as_deref();
        },
        _ => {},
    }

    match (
        request.has_selection.unwrap_or(false),
        request.editable.unwrap_or(false),
    ) {
        (true, true) => Some("selection-field"),
        (true, false) => Some("selection"),
        (false, true) => Some("empty-context"),
        (false, false) => None,
    }
}

fn contextual_assist_anchor_from_request(
    app: &AppHandle,
    request: &ContextualAssistOpenRequest,
) -> Result<(f64, f64), String> {
    match (request.anchor_x, request.anchor_y) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Ok((x, y)),
        _ => contextual_assist_monitor_center_anchor(app)
            .ok_or_else(|| "no display available for contextual assist placement".to_string()),
    }
}

fn contextual_assist_monitor_center_anchor(app: &AppHandle) -> Option<(f64, f64)> {
    let monitors = app.available_monitors().ok()?;
    let monitor = monitors.first()?;
    let position = monitor.position();
    let size = monitor.size();
    Some((
        position.x as f64 + (size.width as f64 / 2.0),
        position.y as f64 + (size.height as f64 / 2.0),
    ))
}

#[derive(Debug)]
struct HostGatewayRequest {
    head: String,
    body: Vec<u8>,
}

async fn read_host_gateway_request_with_deadline(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    deadline: Duration,
) -> Result<HostGatewayRequest, String> {
    timeout(deadline, read_host_gateway_request(stream))
        .await
        .map_err(|_| "host gateway request exceeded its whole-request deadline".to_owned())?
}

async fn read_host_gateway_request(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<HostGatewayRequest, String> {
    let mut buffer = Vec::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let bytes_read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("failed to read request: {error}"))?;
        if bytes_read == 0 {
            return Err("empty host gateway request".to_string());
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if let Some(index) = find_header_end(&buffer) {
            if index > HOST_GATEWAY_HEADER_LIMIT {
                return Err("host gateway request headers too large".to_string());
            }
            break index;
        }
        if buffer.len() > HOST_GATEWAY_HEADER_LIMIT {
            return Err("host gateway request headers too large".to_string());
        }
    };
    let head = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| "host gateway request headers are not UTF-8".to_owned())?
        .to_owned();
    let content_length = strict_content_length_from_headers(&head)?;
    let body_ceiling = host_gateway_route_body_ceiling(&head)?;
    if content_length > body_ceiling {
        return Err(format!(
            "host gateway request body too large: {content_length} bytes"
        ));
    }
    let body_start = header_end + 4;
    let mut body = buffer.get(body_start..).unwrap_or_default().to_vec();
    if body.len() > content_length {
        return Err("host gateway request contains trailing or pipelined bytes".to_owned());
    }
    while body.len() < content_length {
        let remaining = content_length - body.len();
        let read_len = remaining.min(chunk.len());
        let bytes_read = stream
            .read(&mut chunk[..read_len])
            .await
            .map_err(|error| format!("failed to read request body: {error}"))?;
        if bytes_read == 0 {
            return Err("host gateway request body ended before content-length".to_owned());
        }
        body.extend_from_slice(&chunk[..bytes_read]);
    }
    Ok(HostGatewayRequest { head, body })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn strict_content_length_from_headers(head: &str) -> Result<usize, String> {
    let mut content_length = None;
    for line in head.lines().skip(1) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| "host gateway request header is malformed".to_owned())?;
        if name.trim().eq_ignore_ascii_case("transfer-encoding") {
            return Err("host gateway transfer-encoding is unsupported".to_owned());
        }
        if name.trim().eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err("host gateway request has duplicate content-length".to_owned());
            }
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|error| format!("invalid content-length header: {error}"))?,
            );
        }
    }
    Ok(content_length.unwrap_or(0))
}

fn single_header_value(head: &str, expected_name: &str) -> Option<String> {
    let mut value = None;
    for line in head.lines().skip(1) {
        let (name, candidate) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case(expected_name) {
            if value.is_some() {
                return None;
            }
            value = Some(candidate.trim().to_owned());
        }
    }
    value
}

fn trusted_local_settings_origin(head: &str, config: &MagicianDesktopConfig) -> Option<String> {
    let origin = single_header_value(head, "origin")?;
    let parsed = reqwest::Url::parse(&origin).ok()?;
    if parsed.scheme() != "http"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]") {
        return None;
    }
    let port = parsed.port_or_known_default()?;
    let configured_ui_port = reqwest::Url::parse(config.host_gateway.ui_url.trim())
        .ok()
        .and_then(|url| url.port_or_known_default());
    if port != config.network.magician_port
        && port != config.network.magicutor_port
        && Some(port) != configured_ui_port
    {
        return None;
    }
    Some(origin)
}

fn host_gateway_route_body_ceiling(head: &str) -> Result<usize, String> {
    let first_line = head
        .lines()
        .next()
        .ok_or_else(|| "host gateway request line is absent".to_owned())?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if method.is_empty()
        || path.is_empty()
        || !version.starts_with("HTTP/1.")
        || parts.next().is_some()
    {
        return Err("host gateway request line is malformed".to_owned());
    }
    let normalized_path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    if normalized_path.starts_with("/host/apps/macos/")
        || normalized_path.starts_with("/host/apps/android/")
    {
        Ok(crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES)
    } else if matches!(
        normalized_path,
        "/host/speech/transcribe" | "/host/speech/synthesize"
    ) {
        Ok(HOST_GATEWAY_MAX_BODY_BYTES)
    } else {
        Ok(HOST_GATEWAY_DEFAULT_BODY_BYTES)
    }
}

async fn handle_speech_transcribe(
    app: &AppHandle,
    body: &[u8],
) -> Result<HostSpeechTranscribeResponse, String> {
    let request: HostSpeechTranscribeRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid speech transcription payload: {error}"))?;
    let audio = BASE64
        .decode(request.audio_b64.as_bytes())
        .map_err(|error| format!("invalid base64 audio payload: {error}"))?;
    if audio.is_empty() {
        return Err("empty audio payload".to_string());
    }
    transcribe_speech_audio(
        app,
        audio,
        request.filename,
        request.content_type,
        request.language,
        request.message_id,
    )
    .await
}

async fn handle_speech_synthesize(
    app: &AppHandle,
    body: &[u8],
) -> Result<HostSpeechSynthesizeResponse, String> {
    let request: HostSpeechSynthesizeRequest = serde_json::from_slice(body)
        .map_err(|error| format!("invalid speech synthesis payload: {error}"))?;
    synthesize_speech_audio(app, request).await
}

pub(crate) async fn transcribe_speech_audio(
    app: &AppHandle,
    audio: Vec<u8>,
    filename: Option<String>,
    content_type: String,
    language: Option<String>,
    message_id: Option<String>,
) -> Result<HostSpeechTranscribeResponse, String> {
    if audio.is_empty() {
        return Err("empty audio payload".to_string());
    }
    let config = cloned_config(app).await;
    let helper = resolve_speech_helper_binary(&config);
    let audio_path =
        write_temp_speech_audio(&audio, filename.as_deref(), content_type.as_str()).await?;
    let mut command = Command::new(&helper);
    command.arg("transcribe").arg("--file").arg(&audio_path);
    if let Some(language) = language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.arg("--locale").arg(language);
    }
    let helper_timeout = speech_helper_timeout(audio.len());
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match timeout(helper_timeout, command.output()).await {
        Ok(result) => result.map_err(|error| {
            format!(
                "failed to run macOS Speech helper {}: {error}",
                helper.display()
            )
        }),
        Err(_) => {
            warn!(
                "macOS Speech helper timed out helper={} bytes={} timeout_ms={}",
                helper.display(),
                audio.len(),
                helper_timeout.as_millis()
            );
            Err(format!(
                "macOS Speech helper timed out after {}ms",
                helper_timeout.as_millis()
            ))
        },
    };
    let _ = tokio::fs::remove_file(&audio_path).await;
    let output = output?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(format!(
            "macOS Speech helper failed status={} stderr={} stdout={}",
            output.status, stderr, stdout
        ));
    }
    let response: HostSpeechTranscribeResponse =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            format!("macOS Speech helper returned invalid JSON: {error}; stdout={stdout}")
        })?;
    if response.transcript.trim().is_empty() {
        return Err("macOS Speech returned an empty transcript".to_string());
    }
    info!(
        "macOS Speech transcription completed bytes={} transcript_len={} language={:?} message_id={:?}",
        audio.len(),
        response.transcript.len(),
        response.language.as_deref(),
        message_id
    );
    Ok(response)
}

pub(crate) async fn get_speech_status(app: &AppHandle) -> HostSpeechStatus {
    let config = cloned_config(app).await;
    speech_status_for_config(&config).await
}

async fn speech_status_for_config(config: &MagicianDesktopConfig) -> HostSpeechStatus {
    let helper = resolve_speech_helper_binary(config);
    let helper_exists = helper.is_file();
    if !config.host_gateway.enabled {
        return HostSpeechStatus {
            available: false,
            stt_available: false,
            tts_available: false,
            helper_path: helper.display().to_string(),
            helper_exists,
            stt_authorized: None,
            stt_authorization_status: None,
            reason: Some("host gateway disabled by desktop config".to_string()),
        };
    }
    if !helper_exists {
        return HostSpeechStatus {
            available: false,
            stt_available: false,
            tts_available: false,
            helper_path: helper.display().to_string(),
            helper_exists,
            stt_authorized: None,
            stt_authorization_status: None,
            reason: Some(format!(
                "macOS Speech helper not found at {}",
                helper.display()
            )),
        };
    }

    let auth_status = speech_helper_authorization_status(&helper).await;
    let (stt_authorized, stt_authorization_status, reason) = match auth_status {
        Ok(status) => (
            Some(status.authorized),
            Some(status.status.clone()),
            if status.authorized {
                None
            } else {
                Some(format!(
                    "macOS Speech recognition authorization is {}",
                    status.status
                ))
            },
        ),
        Err(error) => (
            Some(false),
            Some("unknown".to_string()),
            Some(format!("macOS Speech helper status failed: {error}")),
        ),
    };
    let stt_available = stt_authorized.unwrap_or(false);
    HostSpeechStatus {
        available: true,
        stt_available,
        tts_available: true,
        helper_path: helper.display().to_string(),
        helper_exists,
        stt_authorized,
        stt_authorization_status,
        reason,
    }
}

async fn speech_helper_authorization_status(
    helper: &PathBuf,
) -> Result<SpeechAuthorizationOutput, String> {
    let mut command = Command::new(helper);
    command
        .arg("status")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match timeout(Duration::from_secs(5), command.output()).await {
        Ok(result) => result.map_err(|error| {
            format!(
                "failed to run macOS Speech helper {}: {error}",
                helper.display()
            )
        })?,
        Err(_) => return Err("macOS Speech helper status timed out after 5000ms".to_string()),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(format!(
            "macOS Speech helper status failed status={} stderr={} stdout={}",
            output.status, stderr, stdout
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        let stdout = String::from_utf8_lossy(&output.stdout);
        format!("macOS Speech helper status returned invalid JSON: {error}; stdout={stdout}")
    })
}

async fn synthesize_speech_audio(
    app: &AppHandle,
    request: HostSpeechSynthesizeRequest,
) -> Result<HostSpeechSynthesizeResponse, String> {
    let text = request.text.trim();
    if text.is_empty() {
        return Err("empty synthesis text".to_string());
    }
    let config = cloned_config(app).await;
    let helper = resolve_speech_helper_binary(&config);
    let text_path = write_temp_speech_text(text).await?;
    let requested_format = request
        .format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("wav");
    if !requested_format.eq_ignore_ascii_case("wav") {
        debug!(
            requested_format,
            "macOS Speech synthesis currently emits WAV; ignoring requested format"
        );
    }
    let output_path = temp_speech_output_path("wav")?;
    let mut command = Command::new(&helper);
    command
        .arg("synthesize")
        .arg("--text-file")
        .arg(&text_path)
        .arg("--output")
        .arg(&output_path);
    if let Some(voice) = request
        .voice
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        command.arg("--voice").arg(voice);
    }
    if let Some(rate) = request
        .rate
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        command.arg("--rate").arg(format!("{rate:.3}"));
    }

    let helper_timeout = speech_synthesis_timeout(text.len());
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = match timeout(helper_timeout, command.output()).await {
        Ok(result) => result.map_err(|error| {
            format!(
                "failed to run macOS Speech helper {}: {error}",
                helper.display()
            )
        }),
        Err(_) => {
            warn!(
                "macOS Speech synthesis helper timed out helper={} text_len={} timeout_ms={}",
                helper.display(),
                text.len(),
                helper_timeout.as_millis()
            );
            Err(format!(
                "macOS Speech synthesis helper timed out after {}ms",
                helper_timeout.as_millis()
            ))
        },
    };
    let _ = tokio::fs::remove_file(&text_path).await;
    if output.is_err() {
        let _ = tokio::fs::remove_file(&output_path).await;
    }
    let output = output?;
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&output_path).await;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(format!(
            "macOS Speech synthesis helper failed status={} stderr={} stdout={}",
            output.status, stderr, stdout
        ));
    }
    let audio_result = tokio::fs::read(&output_path)
        .await
        .map_err(|error| format!("failed to read macOS Speech synthesis output: {error}"));
    let _ = tokio::fs::remove_file(&output_path).await;
    let audio = audio_result?;
    if audio.is_empty() {
        return Err("macOS Speech synthesis returned empty audio".to_string());
    }
    let mut response: HostSpeechSynthesizeResponse = serde_json::from_slice(&output.stdout)
        .map_err(|error| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            format!("macOS Speech synthesis helper returned invalid JSON: {error}; stdout={stdout}")
        })?;
    response.audio_b64 = BASE64.encode(&audio);
    response.message_id = response.message_id.or(request.message_id);
    if response.model.trim().is_empty() {
        response.model = request
            .model
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "av_speech_synthesizer".to_string());
    }
    info!(
        "macOS Speech synthesis completed text_len={} bytes={} voice={:?} message_id={:?}",
        text.len(),
        audio.len(),
        response.voice.as_deref(),
        response.message_id.as_deref()
    );
    Ok(response)
}

fn speech_helper_timeout(audio_bytes: usize) -> Duration {
    let estimated_seconds =
        (audio_bytes / SPEECH_HELPER_BYTES_PER_SECOND_ESTIMATE).saturating_mul(3) + 12;
    Duration::from_secs(estimated_seconds as u64)
        .max(SPEECH_HELPER_MIN_TIMEOUT)
        .min(SPEECH_HELPER_MAX_TIMEOUT)
}

fn speech_synthesis_timeout(text_bytes: usize) -> Duration {
    let estimated_seconds = (text_bytes / 80).saturating_add(8);
    Duration::from_secs(estimated_seconds as u64)
        .max(SPEECH_SYNTHESIS_MIN_TIMEOUT)
        .min(SPEECH_SYNTHESIS_MAX_TIMEOUT)
}

async fn write_temp_speech_text(text: &str) -> Result<PathBuf, String> {
    let path = temp_speech_base_path("speech-text", "txt")?;
    tokio::fs::write(&path, text)
        .await
        .map_err(|error| format!("failed to write speech temp text: {error}"))?;
    Ok(path)
}

fn temp_speech_output_path(ext: &str) -> Result<PathBuf, String> {
    temp_speech_base_path("speech-output", ext)
}

fn temp_speech_base_path(prefix: &str, ext: &str) -> Result<PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push("magician-macos-speech");
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("failed to create speech temp dir: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(dir.join(format!("{prefix}-{}-{stamp}.{ext}", std::process::id())))
}

async fn write_temp_speech_audio(
    audio: &[u8],
    filename: Option<&str>,
    content_type: &str,
) -> Result<PathBuf, String> {
    let ext = speech_audio_extension(filename, content_type);
    let path = temp_speech_base_path("speech", ext)?;
    tokio::fs::write(&path, audio)
        .await
        .map_err(|error| format!("failed to write speech temp audio: {error}"))?;
    Ok(path)
}

fn speech_audio_extension(filename: Option<&str>, content_type: &str) -> &'static str {
    if let Some(filename) = filename {
        let lower = filename.to_ascii_lowercase();
        if lower.ends_with(".wav") {
            return "wav";
        }
        if lower.ends_with(".m4a") || lower.ends_with(".mp4") {
            return "m4a";
        }
        if lower.ends_with(".aiff") || lower.ends_with(".aif") {
            return "aiff";
        }
    }
    let lower = content_type.to_ascii_lowercase();
    if lower.contains("wav") {
        "wav"
    } else if lower.contains("mp4") || lower.contains("m4a") {
        "m4a"
    } else if lower.contains("aiff") || lower.contains("aif") {
        "aiff"
    } else {
        "audio"
    }
}

async fn write_json<T: Serialize>(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &T,
) -> Result<(), String> {
    let body = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason,
        body.len()
    );
    timeout(HOST_GATEWAY_RESPONSE_WRITE_TIMEOUT, async {
        stream
            .write_all(header.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        stream
            .write_all(&body)
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "host gateway response write exceeded its deadline".to_owned())?
}

async fn write_cors_json<T: Serialize>(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &T,
    allowed_origin: &str,
) -> Result<(), String> {
    let body = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: {}\r\nVary: Origin\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason,
        allowed_origin,
        body.len()
    );
    timeout(HOST_GATEWAY_RESPONSE_WRITE_TIMEOUT, async {
        stream
            .write_all(header.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        stream
            .write_all(&body)
            .await
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "host gateway response write exceeded its deadline".to_owned())?
}

async fn write_error(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    error: &str,
) -> Result<(), String> {
    write_json(
        stream,
        code,
        reason,
        &json!({
            "ok": false,
            "error": error,
        }),
    )
    .await
}

async fn post_presence_control(command: &str, body: &str) -> Result<(), String> {
    let mut last_error = None;
    for _ in 0..20 {
        match post_presence_control_once_with_body(command, body).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_millis(75)).await;
            },
        }
    }
    Err(last_error.unwrap_or_else(|| "macOS presence control did not respond".to_string()))
}

async fn post_presence_control_once_with_body(command: &str, body: &str) -> Result<(), String> {
    let command_path = match command {
        "ask"
        | "tickle"
        | "summon"
        | "glide-to"
        | "dock"
        | "quiet"
        | "visible"
        | "replay-startup"
        | "roam"
        | "follow-mouse"
        | "recording/start"
        | "recording/stop"
        | "bubble"
        | "voice-bubble"
        | "voice-state"
        | "show-voice-messages"
        | "clear-voice-messages"
        | "quit" => format!("/control/{command}"),
        other => return Err(format!("unknown macOS presence control command: {other}")),
    };
    let addr = format!("127.0.0.1:{}", DEFAULT_MACOS_PRESENCE_CONTROL_PORT);
    let mut stream = TcpStream::connect(&addr).await.map_err(|error| {
        format!("failed to connect to macOS presence control at {addr}: {error}")
    })?;
    let body_bytes = body.as_bytes();
    let request = format!(
        "POST {command_path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
        body_bytes.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("failed to write macOS presence control request: {error}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|error| format!("failed to read macOS presence control response: {error}"))?;
    let response = String::from_utf8_lossy(&response);
    if response.starts_with("HTTP/1.1 2") {
        Ok(())
    } else {
        Err(format!("macOS presence control failed: {response}"))
    }
}

/// Glide the mascot to a screen position with an animated transition.
/// Coordinates are passed in **macOS screen-bottom-left** space (NSWindow
/// convention). The HUD WebView reports coordinates in viewport-top-left
/// space; the Tauri command below translates before forwarding.
async fn post_presence_glide_to(x: f64, y: f64) -> Result<(), String> {
    let body = format!("{{\"x\":{x},\"y\":{y}}}");
    let mut last_error = None;
    for _ in 0..20 {
        match post_presence_control_once_with_body("glide-to", &body).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_millis(75)).await;
            },
        }
    }
    Err(last_error.unwrap_or_else(|| "macOS presence control did not respond".to_string()))
}

/// Tauri command: send the mascot to a screen point with animation.
/// Called by the HUD's onMount so the orb visually anchors below the
/// composer when Quick Automate opens.
///
/// Coordinates here are in **macOS NSWindow space** (bottom-left
/// origin). The JS caller is responsible for the Y-flip — the HUD
/// knows its own viewport height via `window.innerHeight` and the
/// Tauri overlay window fills the active monitor, so the JS computes
/// `flippedY = window.innerHeight - cssY` before invoking this.
/// Doing the flip JS-side keeps this command transport-agnostic
/// (no Cocoa wiring required in Rust).
#[tauri::command]
pub async fn glide_mascot_to(x: f64, y: f64) -> Result<(), String> {
    post_presence_glide_to(x, y).await
}

/// Tauri command: send the mascot back to its docked parking position.
#[tauri::command]
pub async fn dock_mascot() -> Result<(), String> {
    post_presence_control("dock", "").await
}

pub(crate) fn resolve_speech_helper_binary(config: &MagicianDesktopConfig) -> PathBuf {
    if let Ok(value) = std::env::var("MAGICIAN_MACOS_SPEECH_HELPER_BIN") {
        let value = value.trim();
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    if !config
        .host_gateway
        .macos_speech_helper_bin
        .trim()
        .is_empty()
    {
        return PathBuf::from(config.host_gateway.macos_speech_helper_bin.trim());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("magician-macos-speech-helper.bin");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    if let Ok(target_dir) = std::env::var("CARGO_TARGET_DIR") {
        let candidate = PathBuf::from(target_dir).join(MACOS_SPEECH_HELPER_TARGET_SUBPATH);
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("magician-macos-speech-helper")
}

#[cfg(test)]
mod tests {
    use super::{
        accessibility_status, automation_is_available, host_gateway_route_body_ceiling,
        read_host_gateway_request_with_deadline, read_typed_app_macos_pipe, runtime_endpoints,
        strict_content_length_from_headers, trusted_local_settings_origin,
        try_reserve_host_gateway_connection, typed_app_macos_gateway_binding_is_exact,
        typed_app_macos_owner_fence_matches, typed_app_macos_tcc_policy_observation,
        validate_apple_reminder_request, HostAppleReminderRequest,
    };
    use crate::config::MagicianDesktopConfig;
    use std::sync::{atomic::AtomicUsize, Arc};
    use tokio::io::AsyncWriteExt;

    #[test]
    fn cua_makes_linux_and_windows_automation_available_without_apple_events() {
        for platform in ["linux", "windows"] {
            assert!(automation_is_available(platform, true, false, false));
            assert!(!automation_is_available(platform, false, true, true));
        }
        assert!(automation_is_available("macos", false, true, true));
    }

    #[test]
    fn a_missing_accessibility_grant_is_reported_with_its_reason() {
        let missing = accessibility_status("macos", false);
        assert_eq!(missing["permitted"], false);
        assert!(missing["reason"].as_str().unwrap().contains("1002"));
        let granted = accessibility_status("macos", true);
        assert_eq!(granted["permitted"], true);
        assert!(granted["reason"].is_null());
        for platform in ["linux", "windows"] {
            assert!(accessibility_status(platform, true)["permitted"].is_null());
        }
    }

    #[test]
    fn runtime_endpoint_discovery_uses_configured_service_ports() {
        let mut config = MagicianDesktopConfig::default();
        config.network.magician_port = 4102;
        config.network.magicutor_port = 4103;

        let endpoints = runtime_endpoints(&config);

        assert_eq!(endpoints.schema_version, 1);
        assert_eq!(
            endpoints.magician_api_base,
            "http://127.0.0.1:4102/api/magician/v2"
        );
        assert_eq!(
            endpoints.magician_health_url,
            "http://127.0.0.1:4102/health"
        );
        assert_eq!(endpoints.magicutor_api_base, "http://127.0.0.1:4103");
        assert_eq!(
            endpoints.magicutor_bridge_url,
            "ws://127.0.0.1:4103/bridge/native"
        );
    }

    #[test]
    fn local_android_observation_route_accepts_only_configured_loopback_web_origins() {
        let mut config = MagicianDesktopConfig::default();
        config.network.magician_port = 4102;
        config.network.magicutor_port = 4103;
        config.host_gateway.ui_url = "http://localhost:4173".to_owned();

        for origin in [
            "http://localhost:4173",
            "http://127.0.0.1:4102",
            "http://[::1]:4103",
        ] {
            let head = format!("POST /host/ui/android-observation HTTP/1.1\r\nOrigin: {origin}");
            assert_eq!(
                trusted_local_settings_origin(&head, &config).as_deref(),
                Some(origin)
            );
        }
        for origin in [
            "https://localhost:4173",
            "http://evil.example:4173",
            "http://localhost:9999",
            "http://localhost:4173/path",
        ] {
            let head = format!("POST /host/ui/android-observation HTTP/1.1\r\nOrigin: {origin}");
            assert!(
                trusted_local_settings_origin(&head, &config).is_none(),
                "{origin}"
            );
        }
        assert!(trusted_local_settings_origin(
            "POST /host/ui/android-observation HTTP/1.1\r\nOrigin: http://localhost:4173\r\nOrigin: http://localhost:4173",
            &config,
        )
        .is_none());
    }

    #[test]
    fn typed_macos_routes_reject_network_or_configured_endpoint_drift() {
        assert!(typed_app_macos_gateway_binding_is_exact("127.0.0.1", 3017));
        assert!(!typed_app_macos_gateway_binding_is_exact("0.0.0.0", 3017));
        assert!(!typed_app_macos_gateway_binding_is_exact("::1", 3017));
        assert!(!typed_app_macos_gateway_binding_is_exact("localhost", 3017));
        assert!(!typed_app_macos_gateway_binding_is_exact("127.0.0.1", 3018));
    }

    #[test]
    fn typed_macos_tcc_policy_digest_is_canonical_and_detects_drift() {
        let first = typed_app_macos_tcc_policy_observation(
            r#"{"permissions":{"screenRecording":false,"axTrusted":true}}"#,
        )
        .expect("first policy");
        let reordered = typed_app_macos_tcc_policy_observation(
            r#"{"axTrusted":"authorized","screenRecording":"denied"}"#,
        )
        .expect("same policy");
        let changed =
            typed_app_macos_tcc_policy_observation(r#"{"axTrusted":true,"screenRecording":true}"#)
                .expect("changed policy");
        assert!(first.accessibility);
        assert!(!first.screen_recording);
        assert_eq!(first.digest, reordered.digest);
        assert_ne!(first.digest, changed.digest);
    }

    #[test]
    fn typed_macos_final_owner_fences_reject_midflight_policy_or_app_drift() {
        let reviewed =
            typed_app_macos_tcc_policy_observation(r#"{"axTrusted":true,"screenRecording":false}"#)
                .expect("reviewed policy");
        let changed_policy =
            typed_app_macos_tcc_policy_observation(r#"{"axTrusted":true,"screenRecording":true}"#)
                .expect("changed policy");
        let expected_app = format!("blake3:{}", "31".repeat(32));
        let changed_app = format!("blake3:{}", "32".repeat(32));

        assert!(typed_app_macos_owner_fence_matches(
            true,
            &reviewed,
            false,
            &expected_app,
            &reviewed.digest,
            &expected_app,
        ));
        assert!(!typed_app_macos_owner_fence_matches(
            true,
            &changed_policy,
            false,
            &expected_app,
            &reviewed.digest,
            &expected_app,
        ));
        assert!(!typed_app_macos_owner_fence_matches(
            true,
            &reviewed,
            false,
            &changed_app,
            &reviewed.digest,
            &expected_app,
        ));
    }

    #[test]
    fn host_gateway_framing_and_route_ceilings_fail_closed_before_allocation() {
        assert!(strict_content_length_from_headers(
            "POST /host/apps/macos/action HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1"
        )
        .is_err());
        assert!(strict_content_length_from_headers(
            "POST /host/apps/macos/action HTTP/1.1\r\nTransfer-Encoding: chunked"
        )
        .is_err());
        assert_eq!(
            host_gateway_route_body_ceiling("POST /host/apps/macos/action HTTP/1.1")
                .expect("typed ceiling"),
            crate::app_macos_host::APP_MACOS_HOST_MAX_REQUEST_BYTES
        );
        assert_eq!(
            host_gateway_route_body_ceiling("POST /host/speech/transcribe HTTP/1.1")
                .expect("speech ceiling"),
            super::HOST_GATEWAY_MAX_BODY_BYTES
        );
    }

    #[tokio::test]
    async fn host_gateway_saturation_and_slow_partial_body_are_bounded() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let retained = try_reserve_host_gateway_connection(&slots).expect("first slot");
        assert!(try_reserve_host_gateway_connection(&slots).is_none());
        drop(retained);
        assert!(try_reserve_host_gateway_connection(&slots).is_some());

        let (mut writer, mut reader) = tokio::io::duplex(256);
        writer
            .write_all(b"POST /host/apps/macos/action HTTP/1.1\r\nContent-Length: 10\r\n\r\none")
            .await
            .expect("partial request");
        let error = read_host_gateway_request_with_deadline(
            &mut reader,
            tokio::time::Duration::from_millis(20),
        )
        .await
        .expect_err("partial body must expire");
        assert!(error.contains("whole-request deadline"));
    }

    #[test]
    fn apple_reminder_request_is_bounded_before_automation() {
        let valid = HostAppleReminderRequest {
            idempotency_key: "receipt-1".to_string(),
            title: "Review policy".to_string(),
            notes: "Review it before booking".to_string(),
            at: "2099-07-20T03:30:00Z".to_string(),
            timezone: Some("Asia/Kolkata".to_string()),
        };
        assert!(validate_apple_reminder_request(&valid).is_ok());

        let empty_idempotency_key = HostAppleReminderRequest {
            idempotency_key: "  ".to_string(),
            ..valid.clone()
        };
        assert_eq!(
            validate_apple_reminder_request(&empty_idempotency_key).unwrap_err(),
            "reminder idempotency_key must be non-empty and contain at most 512 characters"
        );

        let empty_title = HostAppleReminderRequest {
            title: "  ".to_string(),
            ..valid
        };
        assert_eq!(
            validate_apple_reminder_request(&empty_title).unwrap_err(),
            "reminder title is required"
        );
    }

    #[tokio::test]
    async fn typed_macos_pipe_rejects_before_allocating_past_shared_ceiling() {
        let (mut writer, reader) = tokio::io::duplex(32);
        let write = tokio::spawn(async move {
            let _ = writer.write_all(b"eight123").await;
        });
        let result = read_typed_app_macos_pipe(reader, Arc::new(AtomicUsize::new(4))).await;
        let _ = write.await;
        assert_eq!(
            result.unwrap_err(),
            "typed cua-driver exceeded its output byte ceiling"
        );
    }

    /// Live proof of the typed Apps owner's mutation path on CuaDriver 0.28.2
    /// against a scratch TextEdit document. Only pairing and permit issuance
    /// are bypassed: every action still runs through
    /// `execute_typed_app_macos_action` with the real staged-binary,
    /// application-identity, TCC-policy and observation-content digests, so
    /// each owner fence runs as it does in production. The private daemon
    /// inherits the TCC grants of the process that launched the test, so the
    /// terminal running it needs Accessibility (and Screen Recording for the
    /// observation screenshot).
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "live: drives TextEdit through the real CuaDriver 0.28.2 owner"]
    async fn live_textedit_owner_round_trip() {
        use super::{
            current_unix_ms, execute_typed_app_macos_action, handle_typed_app_macos_ax,
            reserve_typed_app_macos_identity_hash_slot, typed_app_macos_daemon_home,
            typed_app_macos_private_daemon, TypedAppMacosDaemonHome,
            APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS, APP_MACOS_TYPED_DAEMON_DIRECTORY,
            APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
        };
        use crate::app_macos_host::{
            app_macos_host_current_application_identity_digest_blocking,
            app_macos_host_stage_binary, AppMacosHostStopSignal, AuthorizedAppMacosHostAction,
        };
        use magician_app_contract::macos_host::{
            AppMacosHostAction, AppMacosHostKey, AppMacosHostScrollDirection,
        };
        use serde_json::Value;
        use std::path::{Path, PathBuf};
        use std::time::Duration;

        const CUA_DRIVER_SOURCE: &str = "/Applications/CuaDriver.app/Contents/MacOS/cua-driver";
        const BUNDLE_ID: &str = "com.apple.TextEdit";
        const DESKTOP_BUNDLE_ID: &str = "ai.magicbeans.magican.desktop";
        const SEED: &str = "magician-live-seed";
        const OBSERVATION_REF: &str = "interactive-observation:live-textedit";
        const EVIDENCE_CEILING: u64 = 16 * 1024 * 1024;
        const RESPONSE_CEILING: usize = 32 * 1024 * 1024;
        const PERMIT_LIFETIME_MS: i64 = 60_000;
        const SETTLE: Duration = Duration::from_millis(400);

        /// Absolute program path, no PATH override: posix_spawn, never fork.
        fn osascript(lines: &[&str]) -> Result<String, String> {
            let mut command = std::process::Command::new("/usr/bin/osascript");
            for line in lines {
                command.arg("-e").arg(line);
            }
            let output = command
                .output()
                .map_err(|error| format!("osascript failed to start: {error}"))?;
            if !output.status.success() {
                return Err(format!(
                    "osascript failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }

        fn textedit_pid() -> Result<u32, String> {
            let output = std::process::Command::new("/usr/bin/pgrep")
                .args(["-x", "TextEdit"])
                .output()
                .map_err(|error| format!("pgrep failed to start: {error}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let pids = stdout
                .lines()
                .filter_map(|line| line.trim().parse::<u32>().ok())
                .collect::<Vec<_>>();
            match pids.as_slice() {
                [pid] => Ok(*pid),
                _ => Err(format!(
                    "expected exactly one TextEdit process, found {pids:?}"
                )),
            }
        }

        struct LiveObservation {
            // The raw `tree_markdown`: each mutation's permit carries the
            // fence digest the runtime would derive from it for that action.
            tree: String,
            elements: Vec<Value>,
            screenshot_scale_millis: Option<u16>,
        }

        impl LiveObservation {
            fn parse(result: &Value) -> Result<Self, String> {
                let stdout = result
                    .get("observation")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "observe result lacks an observation".to_owned())?;
                let observation: Value = serde_json::from_str(stdout)
                    .map_err(|error| format!("observation is not JSON: {error}"))?;
                let tree = observation
                    .get("tree_markdown")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "observation lacks tree_markdown".to_owned())?;
                let elements = observation
                    .get("elements")
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| "observation lacks elements[]".to_owned())?;
                let screenshot_scale_millis = observation
                    .get("screenshot_scale")
                    .and_then(Value::as_f64)
                    .filter(|scale| scale.is_finite())
                    .map(|scale| (scale * 1_000.0).round())
                    .filter(|millis| (500.0..=4_000.0).contains(millis))
                    .map(|millis| millis as u16);
                Ok(Self {
                    tree: tree.to_owned(),
                    elements,
                    screenshot_scale_millis,
                })
            }

            /// The element indexes an observed action is fenced on, as the
            /// runtime derives them: the token's index for click/type/scroll,
            /// both drag endpoints in order, none (the window fence) for a key.
            fn fence_indexes(action: &AppMacosHostAction) -> Result<Vec<u32>, String> {
                let index = |token: &str| {
                    magician_app_contract::macos_host::app_macos_host_parse_element_token(token)
                        .map(|(_, index)| index)
                        .ok_or_else(|| format!("malformed element token {token:?}"))
                };
                match action {
                    AppMacosHostAction::ClickElement { element_token, .. }
                    | AppMacosHostAction::TypeText { element_token, .. }
                    | AppMacosHostAction::ScrollElement { element_token, .. } => {
                        Ok(vec![index(element_token)?])
                    },
                    AppMacosHostAction::DragElements {
                        source_element_token,
                        destination_element_token,
                        ..
                    } => Ok(vec![
                        index(source_element_token)?,
                        index(destination_element_token)?,
                    ]),
                    AppMacosHostAction::PressKey { .. } => Ok(Vec::new()),
                    _ => Err("action is not fenced on an observation".to_owned()),
                }
            }

            fn fence_text(&self, indexes: &[u32]) -> Result<String, String> {
                magician_app_contract::macos_host::app_macos_host_observation_fence_input(
                    &self.tree, indexes,
                )
                .ok_or_else(|| format!("observation has no fence for elements {indexes:?}"))
            }

            fn fence_digest(&self, indexes: &[u32]) -> Result<String, String> {
                magician_app_contract::macos_host::app_macos_host_observation_fence_digest(
                    &self.tree, indexes,
                )
                .ok_or_else(|| format!("observation has no fence for elements {indexes:?}"))
            }

            fn text_area(&self) -> Result<&serde_json::Map<String, Value>, String> {
                self.elements
                    .iter()
                    .filter_map(Value::as_object)
                    .find(|element| {
                        element.get("role").and_then(Value::as_str) == Some("AXTextArea")
                    })
                    .ok_or_else(|| "observation has no AXTextArea element".to_owned())
            }

            fn text_area_token(&self) -> Result<String, String> {
                self.text_area()?
                    .get("element_token")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "AXTextArea element lacks an element_token".to_owned())
            }

            fn text_area_value(&self) -> Result<String, String> {
                Ok(self
                    .text_area()?
                    .get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned())
            }

            fn frame(element: &serde_json::Map<String, Value>) -> Option<(f64, f64, f64, f64)> {
                let frame = element.get("frame")?.as_object()?;
                let read = |name: &str| frame.get(name).and_then(Value::as_f64);
                let (x, y, w, h) = (read("x")?, read("y")?, read("w")?, read("h")?);
                (w >= 2.0 && h >= 2.0).then_some((x, y, w, h))
            }

            /// Two framed element tokens whose centres lie inside the window
            /// (row 0, the AXWindow): the text area, and a distinct non-control
            /// element when one exists, else the text area again.
            fn drag_pair(&self) -> Option<(String, String)> {
                let rows: Vec<&serde_json::Map<String, Value>> =
                    self.elements.iter().filter_map(Value::as_object).collect();
                let index = |row: &serde_json::Map<String, Value>| {
                    row.get("element_index").and_then(Value::as_u64)
                };
                let role = |row: &serde_json::Map<String, Value>| {
                    row.get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                let window_row = rows.iter().copied().find(|row| index(row) == Some(0))?;
                if role(window_row) != "AXWindow" {
                    return None;
                }
                let window = Self::frame(window_row)?;
                let inside = |row: &serde_json::Map<String, Value>| {
                    Self::frame(row).is_some_and(|(x, y, w, h)| {
                        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                        cx >= window.0
                            && cy >= window.1
                            && cx < window.0 + window.2
                            && cy < window.1 + window.3
                    })
                };
                let token = |row: &serde_json::Map<String, Value>| {
                    row.get("element_token")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
                let source = rows
                    .iter()
                    .copied()
                    .find(|row| role(row) == "AXTextArea" && inside(row))?;
                let source_token = token(source)?;
                let destination_token = rows
                    .iter()
                    .copied()
                    .filter(|row| {
                        index(row) != Some(0)
                            && index(row) != index(source)
                            && !matches!(
                                role(row).as_str(),
                                "AXButton"
                                    | "AXMenuButton"
                                    | "AXPopUpButton"
                                    | "AXCheckBox"
                                    | "AXRadioButton"
                                    | "AXMenuBarItem"
                            )
                            && inside(row)
                    })
                    .find_map(token)
                    .unwrap_or_else(|| source_token.clone());
                Some((source_token, destination_token))
            }
        }

        struct LiveOwner {
            binary: PathBuf,
            binary_digest: String,
            application_identity_digest: String,
            tcc_policy_digest: String,
            process_id: u32,
            window_id: u32,
        }

        impl LiveOwner {
            async fn execute(
                &self,
                action: AppMacosHostAction,
                observation_content_digest: Option<String>,
            ) -> Result<Value, String> {
                let authorized = AuthorizedAppMacosHostAction::for_live_owner_test(
                    action,
                    self.application_identity_digest.clone(),
                    self.tcc_policy_digest.clone(),
                    observation_content_digest,
                    EVIDENCE_CEILING,
                    RESPONSE_CEILING,
                    current_unix_ms()? + PERMIT_LIFETIME_MS,
                    self.binary.clone(),
                    self.binary_digest.clone(),
                );
                let lowered = authorized.lower().map_err(|error| error.to_string())?;
                let identity_hash_slot = reserve_typed_app_macos_identity_hash_slot(
                    &lowered.stop_signal(),
                    lowered.expires_at_ms(),
                )
                .await?;
                execute_typed_app_macos_action(lowered, identity_hash_slot).await
            }

            async fn observe(&self, label: &str) -> Result<LiveObservation, String> {
                tokio::time::sleep(SETTLE).await;
                let result = self
                    .execute(
                        AppMacosHostAction::Observe {
                            bundle_id: BUNDLE_ID.to_owned(),
                            process_id: self.process_id,
                            window_id: self.window_id,
                        },
                        None,
                    )
                    .await;
                match &result {
                    Ok(value) => eprintln!(
                        "[live-textedit] observe after {label}: outcome={}",
                        value.get("outcome").unwrap_or(&Value::Null)
                    ),
                    Err(error) => eprintln!("[live-textedit] observe after {label}: error={error}"),
                }
                let observation = LiveObservation::parse(&result?)?;
                eprintln!(
                    "[live-textedit]   elements={} tree_lines={} text_area_value={:?}",
                    observation.elements.len(),
                    observation.tree.lines().count(),
                    observation.text_area_value().ok()
                );
                Ok(observation)
            }

            /// Run one mutation. An observed action's permit carries the fence
            /// digest of `fenced_by` for the element(s) it names.
            async fn mutate(
                &self,
                verb: &str,
                action: AppMacosHostAction,
                fenced_by: Option<&LiveObservation>,
            ) -> Result<(), String> {
                let fence = match fenced_by {
                    Some(observed) => {
                        let indexes = LiveObservation::fence_indexes(&action)?;
                        let digest = observed
                            .fence_digest(&indexes)
                            .map_err(|error| format!("{verb}: {error}"))?;
                        Some((observed, indexes, digest))
                    },
                    None => None,
                };
                let result = self
                    .execute(action, fence.as_ref().map(|(_, _, digest)| digest.clone()))
                    .await;
                match &result {
                    Ok(value) => eprintln!(
                        "[live-textedit] {verb}: outcome={}",
                        value.get("outcome").unwrap_or(&Value::Null)
                    ),
                    Err(error) => eprintln!("[live-textedit] {verb}: error={error}"),
                }
                let fence_moved =
                    matches!(&result, Err(error) if error.contains("observation changed"));
                if let (true, Some((observed, indexes, _))) = (fence_moved, &fence) {
                    if let Ok(now) = self.observe("fence mismatch").await {
                        let before = observed.fence_text(indexes).unwrap_or_default();
                        let now = now.fence_text(indexes).unwrap_or_else(|error| error);
                        eprintln!("[live-textedit]   fence diff for elements {indexes:?}:");
                        let before: Vec<&str> = before.lines().collect();
                        let now: Vec<&str> = now.lines().collect();
                        for line in before.iter().filter(|line| !now.contains(line)).take(10) {
                            eprintln!("[live-textedit]   - {line}");
                        }
                        for line in now.iter().filter(|line| !before.contains(line)).take(10) {
                            eprintln!("[live-textedit]   + {line}");
                        }
                    }
                }
                let value = result.map_err(|error| format!("{verb}: {error}"))?;
                if value.get("outcome").and_then(Value::as_str) != Some("completed") {
                    return Err(format!("{verb}: outcome was not completed: {value}"));
                }
                Ok(())
            }
        }

        async fn live_round_trip(owner_root: &Path) -> Result<(), String> {
            // Harness setup, not the owner: a scratch document and its window.
            let make_seed_document = format!(
                "set seedDocument to make new document with properties {{text:\"{SEED}\"}}"
            );
            let window_id = osascript(&[
                "tell application \"TextEdit\"",
                "activate",
                make_seed_document.as_str(),
                "delay 1",
                "return id of (first window whose name is (name of seedDocument))",
                "end tell",
            ])?
            .parse::<u32>()
            .map_err(|error| format!("TextEdit window id is not a number: {error}"))?;
            let process_id = textedit_pid()?;
            eprintln!("[live-textedit] TextEdit pid={process_id} window_id={window_id}");

            let (binary, binary_digest) =
                app_macos_host_stage_binary(Path::new(CUA_DRIVER_SOURCE), owner_root)
                    .ok_or_else(|| "CuaDriver could not be staged".to_owned())?;
            eprintln!(
                "[live-textedit] staged {} ({binary_digest})",
                binary.display()
            );
            typed_app_macos_daemon_home()
                .set(TypedAppMacosDaemonHome {
                    directory: owner_root.join(APP_MACOS_TYPED_DAEMON_DIRECTORY),
                    host_bundle_id: DESKTOP_BUNDLE_ID.to_owned(),
                })
                .map_err(|_| "typed macOS daemon home was already initialized".to_owned())?;

            let application_identity_digest = tokio::task::spawn_blocking(move || {
                app_macos_host_current_application_identity_digest_blocking(
                    BUNDLE_ID,
                    Some(process_id),
                )
            })
            .await
            .map_err(|_| "application identity hashing was interrupted".to_owned())?
            .ok_or_else(|| "TextEdit application identity is unavailable".to_owned())?;

            // The same check_permissions call, through the same private daemon
            // and staged binary, that the owner's TCC fence makes.
            let permissions = handle_typed_app_macos_ax(
                &binary,
                &binary_digest,
                "check_permissions",
                APP_MACOS_TYPED_CHECK_PERMISSIONS_ARGS,
                APP_MACOS_TYPED_PREFLIGHT_OUTPUT_CEILING,
                &AppMacosHostStopSignal::new(),
                current_unix_ms()? + PERMIT_LIFETIME_MS,
            )
            .await?;
            let tcc_policy = typed_app_macos_tcc_policy_observation(&permissions.stdout)?;
            eprintln!(
                "[live-textedit] check_permissions ok={} accessibility={} screen_recording={}",
                permissions.ok, tcc_policy.accessibility, tcc_policy.screen_recording
            );
            if !permissions.ok || !tcc_policy.accessibility {
                return Err(format!(
                    "Accessibility is not granted to the process running this test: {}",
                    permissions.stdout
                ));
            }

            let owner = LiveOwner {
                binary,
                binary_digest,
                application_identity_digest,
                tcc_policy_digest: tcc_policy.digest,
                process_id,
                window_id,
            };
            let observed = owner.observe("setup").await?;
            observed.text_area_token()?;

            owner
                .mutate(
                    "bring_to_front",
                    AppMacosHostAction::Focus {
                        bundle_id: BUNDLE_ID.to_owned(),
                        process_id,
                    },
                    None,
                )
                .await?;
            let observed = owner.observe("bring_to_front").await?;

            owner
                .mutate(
                    "click",
                    AppMacosHostAction::ClickElement {
                        bundle_id: BUNDLE_ID.to_owned(),
                        process_id,
                        window_id,
                        observation_ref: OBSERVATION_REF.to_owned(),
                        element_token: observed.text_area_token()?,
                        click_count: 1,
                    },
                    Some(&observed),
                )
                .await?;
            let observed = owner.observe("click").await?;

            owner
                .mutate(
                    "type_text",
                    AppMacosHostAction::TypeText {
                        bundle_id: BUNDLE_ID.to_owned(),
                        process_id,
                        window_id,
                        observation_ref: OBSERVATION_REF.to_owned(),
                        element_token: observed.text_area_token()?,
                        text: "cua-live-ok".to_owned(),
                    },
                    Some(&observed),
                )
                .await?;
            let observed = owner.observe("type_text").await?;
            let typed = observed.text_area_value()?;
            if !typed.contains("cua-live-ok") {
                return Err(format!("type_text: text area holds {typed:?}"));
            }

            owner
                .mutate(
                    "press_key",
                    AppMacosHostAction::PressKey {
                        bundle_id: BUNDLE_ID.to_owned(),
                        process_id,
                        window_id,
                        observation_ref: OBSERVATION_REF.to_owned(),
                        key: AppMacosHostKey::Backspace,
                        modifiers: Vec::new(),
                    },
                    Some(&observed),
                )
                .await?;
            let observed = owner.observe("press_key").await?;
            let erased = observed.text_area_value()?;
            if !erased.contains("cua-live-o") || erased.contains("cua-live-ok") {
                return Err(format!("press_key: text area holds {erased:?}"));
            }

            owner
                .mutate(
                    "scroll",
                    AppMacosHostAction::ScrollElement {
                        bundle_id: BUNDLE_ID.to_owned(),
                        process_id,
                        window_id,
                        observation_ref: OBSERVATION_REF.to_owned(),
                        element_token: observed.text_area_token()?,
                        direction: AppMacosHostScrollDirection::Down,
                        amount: 3,
                    },
                    Some(&observed),
                )
                .await?;
            let observed = owner.observe("scroll").await?;

            match observed.drag_pair() {
                Some((source_element_token, destination_element_token)) => {
                    let screenshot_scale_millis =
                        observed.screenshot_scale_millis.unwrap_or_else(|| {
                            eprintln!(
                                "[live-textedit] no screenshot_scale observed; drag assumes 2x"
                            );
                            2_000
                        });
                    eprintln!(
                        "[live-textedit] drag {source_element_token} -> \
                         {destination_element_token} at scale {screenshot_scale_millis}"
                    );
                    owner
                        .mutate(
                            "drag",
                            AppMacosHostAction::DragElements {
                                bundle_id: BUNDLE_ID.to_owned(),
                                process_id,
                                window_id,
                                observation_ref: OBSERVATION_REF.to_owned(),
                                source_element_token,
                                destination_element_token,
                                screenshot_scale_millis,
                            },
                            Some(&observed),
                        )
                        .await?;
                },
                None => eprintln!(
                    "[live-textedit] drag skipped: no framed AXTextArea inside the AXWindow row"
                ),
            }
            Ok(())
        }

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
            % 1_000_000;
        // Canonical (/private/var/...) so staging's no-symlink parent check
        // passes; short so `<root>/cua-run/d.sock` fits the socket-path limit.
        let owner_root = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temp dir")
            .join(format!("mgc-live-{}-{unique}", std::process::id()));

        let outcome = live_round_trip(&owner_root).await;

        // TextEdit cannot filter documents by text in a `whose` clause, so the
        // seed documents are found one by one (backwards, as closing reindexes).
        let seed_test = format!("if (text of d) contains \"{SEED}\" then close d saving no");
        if let Err(error) = osascript(&[
            "tell application \"TextEdit\"",
            "repeat with i from (count of documents) to 1 by -1",
            "set d to document i",
            seed_test.as_str(),
            "end repeat",
            "end tell",
        ]) {
            eprintln!("[live-textedit] cleanup: {error}");
        }
        if let Some(mut daemon) = typed_app_macos_private_daemon().lock().await.take() {
            let _ = daemon.child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), daemon.child.wait()).await;
        }
        let _ = std::fs::remove_dir_all(&owner_root);

        if let Err(error) = outcome {
            panic!("live TextEdit owner round trip failed: {error}");
        }
    }
}
