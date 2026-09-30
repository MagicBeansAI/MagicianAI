use serde::{Deserialize, Serialize};
use std::{path::Path, process::Stdio, time::Duration};
use tauri::AppHandle;
#[cfg(target_os = "macos")]
use tauri::Manager;
use tokio::{process::Command, time::timeout};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Missing,
    Unknown,
    #[allow(dead_code)]
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
pub struct DesktopPermission {
    pub key: String,
    pub title: String,
    pub description: String,
    pub state: PermissionState,
    pub required: bool,
    pub settings_label: String,
    pub details: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CuaSetupReadiness {
    pub installed: bool,
    pub ready: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct PermissionStatusInputs {
    voice_enabled: bool,
    gesture_enabled: bool,
    contextual_assist_enabled: bool,
    speech_required: bool,
    microphone: PermissionState,
    input_monitoring: PermissionState,
    accessibility: PermissionState,
    speech_recognition: PermissionState,
    imessage_enabled: bool,
    full_disk_access: PermissionState,
    automation_messages: PermissionState,
}

#[derive(Debug, Deserialize)]
struct SpeechHelperAuthorizationOutput {
    status: String,
}

#[tauri::command]
pub async fn get_desktop_permissions(app: AppHandle) -> Result<Vec<DesktopPermission>, String> {
    Ok(build_permission_statuses(&app).await)
}

#[tauri::command]
pub async fn open_desktop_permission_settings(permission_key: String) -> Result<(), String> {
    let url = settings_url_for_permission(&permission_key)
        .ok_or_else(|| format!("Unknown desktop permission '{permission_key}'"))?;
    open::that(url)
        .map_err(|error| format!("Failed to open desktop setup for '{permission_key}': {error}"))
}

#[tauri::command]
pub async fn request_desktop_permission(
    app: AppHandle,
    permission_key: String,
) -> Result<(), String> {
    match permission_key.as_str() {
        "microphone" => request_microphone_permission().await,
        "speech_recognition" => request_speech_recognition_permission(app).await,
        "accessibility" => {
            if crate::voice_gesture::request_accessibility_access() {
                Ok(())
            } else {
                open::that(
                    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
                )
                .map_err(|error| {
                    format!(
                        "Magican still needs Accessibility access, and the settings pane could not be opened: {error}"
                    )
                })
            }
        },
        "cua_driver" => request_cua_driver_permission().await,
        "input_monitoring" | "full_disk_access" | "automation_messages" => Ok(()),
        _ => Err(format!("Unknown desktop permission '{permission_key}'")),
    }
}

async fn request_microphone_permission() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        macos::request_microphone_permission().await
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

async fn build_permission_statuses(app: &AppHandle) -> Vec<DesktopPermission> {
    let mut permissions = {
        #[cfg(target_os = "macos")]
        {
            macos::build_permission_statuses(app).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = app;
            Vec::new()
        }
    };
    permissions.push(live_cua_permission_status().await);
    permissions
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct CuaPermissionProbe {
    accessibility_granted: bool,
    screen_recording_granted: bool,
    /// `ready`, `not_checked`, `blocked_by_screen_recording`, … — the
    /// read-only check never probes live capture, so it reports `not_checked`
    /// until `cua-driver permissions grant` has verified it.
    direct_capture_status: Option<String>,
    /// The live ScreenCaptureKit probe; `None` when it did not run.
    screen_recording_capturable: Option<bool>,
    report: String,
}

#[cfg(target_os = "macos")]
impl CuaPermissionProbe {
    fn granted(&self) -> bool {
        // A TCC grant that a live capture just failed is not a usable grant.
        self.accessibility_granted
            && self.screen_recording_granted
            && self.screen_recording_capturable != Some(false)
    }

    fn direct_capture_detail(&self) -> String {
        match (
            self.direct_capture_status.as_deref(),
            self.screen_recording_capturable,
        ) {
            (_, Some(false)) => {
                "A live screen capture failed; run Grant & Verify (`cua-driver permissions grant`) to repair it."
                    .to_string()
            },
            (Some("ready"), _) => "Direct screen capture is verified.".to_string(),
            (Some("not_checked") | None, _) => {
                "Direct screen capture has not been verified yet; `cua-driver permissions grant` verifies it."
                    .to_string()
            },
            (Some(status), _) => format!(
                "Direct screen capture status: {status}. `cua-driver permissions grant` requests and verifies it."
            ),
        }
    }
}

async fn live_cua_permission_status() -> DesktopPermission {
    let Some(binary) = runtime_core::cua::driver_binary() else {
        return cua_permission_status(false, runtime_core::cua::has_desktop_session());
    };
    if !runtime_core::cua::has_desktop_session() {
        return cua_permission_status(true, false);
    }
    if !crate::host_gateway::cua_driver_daemon_running(&binary).await {
        return cua_permission_status(true, true);
    }
    #[cfg(target_os = "macos")]
    {
        match probe_cua_permissions(&binary).await {
            Ok(probe) if probe.granted() => cua_permission_row(
                PermissionState::Granted,
                "Verify Again",
                format!(
                    "The signed CuaDriver.app daemon reports Accessibility and Screen Recording access. {}",
                    probe.direct_capture_detail()
                ),
            ),
            Ok(probe) => cua_permission_row(
                PermissionState::Missing,
                "Grant & Verify",
                format!(
                    "CuaDriver.app is running but still needs macOS access. Grant & Verify runs `cua-driver permissions grant`, which launches CuaDriver.app so macOS attributes the grants to it. {} {}",
                    probe.direct_capture_detail(),
                    probe.report
                ),
            ),
            Err(error) => cua_permission_row(
                PermissionState::Unknown,
                "Start & Verify",
                format!("CuaDriver.app is running, but its permission check failed: {error}"),
            ),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        match probe_cua_desktop_observation(&binary).await {
            Ok((true, report)) => cua_permission_row(
                PermissionState::Granted,
                "Verify Again",
                format!("CuaDriver observed the signed-in desktop session. {report}"),
            ),
            Ok((false, report)) => cua_permission_row(
                PermissionState::Missing,
                "Verify Again",
                format!(
                    "CuaDriver is running but could not observe a desktop window. Check the graphical session and platform accessibility prerequisites. {report}"
                ),
            ),
            Err(error) => cua_permission_row(
                PermissionState::Unknown,
                "Start & Verify",
                format!("CuaDriver is running, but desktop verification failed: {error}"),
            ),
        }
    }
}

pub(crate) async fn cua_setup_readiness() -> CuaSetupReadiness {
    let installed = runtime_core::cua::driver_binary().is_some();
    let status = live_cua_permission_status().await;
    CuaSetupReadiness {
        installed,
        ready: status.state == PermissionState::Granted,
        detail: status.details,
    }
}

fn cua_permission_row(
    state: PermissionState,
    settings_label: &str,
    details: String,
) -> DesktopPermission {
    DesktopPermission {
        key: "cua_driver".to_string(),
        title: "Computer use (CuaDriver)".to_string(),
        description: "Runs computer-use actions in this computer's signed-in desktop session."
            .to_string(),
        state,
        required: false,
        settings_label: settings_label.to_string(),
        details,
    }
}

fn cua_permission_status(installed: bool, desktop_session: bool) -> DesktopPermission {
    let (state, details) = match (installed, desktop_session) {
        (true, true) => (
            PermissionState::Unknown,
            if cfg!(target_os = "macos") {
                "CuaDriver is installed and an interactive desktop session is available. Start its signed app-owned daemon and verify Accessibility and Screen Recording here."
                    .to_string()
            } else {
                "CuaDriver is installed and an interactive desktop session is available. Start its local daemon and verify real window observation here."
                    .to_string()
            },
        ),
        (true, false) => (
            PermissionState::Missing,
            "CuaDriver is installed, but no interactive desktop session is visible. Start Magican Desktop as the signed-in desktop user."
                .to_string(),
        ),
        (false, _) => (
            PermissionState::Missing,
            "Install the pinned CuaDriver from Magican onboarding (Desktop computer use) or `make setup-cua-driver ARGS=--start`, then restart Magican Desktop so the local host gateway can expose computer use."
                .to_string(),
        ),
    };
    DesktopPermission {
        key: "cua_driver".to_string(),
        title: "Computer use (CuaDriver)".to_string(),
        description: "Runs computer-use actions in this computer's signed-in desktop session."
            .to_string(),
        state,
        required: false,
        settings_label: if installed {
            "Start & Verify".to_string()
        } else {
            "Open CUA Setup Guide".to_string()
        },
        details,
    }
}

#[cfg(target_os = "macos")]
async fn probe_cua_permissions(binary: &Path) -> Result<CuaPermissionProbe, String> {
    let mut command = Command::new(binary);
    command
        .args(["call", "check_permissions", "{}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(15), command.output())
        .await
        .map_err(|_| "permission check timed out after 15 seconds".to_string())?
        .map_err(|error| format!("failed to run {}: {error}", binary.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let report = [stdout.as_str(), stderr.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !output.status.success() && report.is_empty() {
        return Err(format!("permission check exited with {}", output.status));
    }
    Ok(parse_cua_permission_report(report))
}

#[cfg(target_os = "macos")]
fn parse_cua_permission_report(report: String) -> CuaPermissionProbe {
    // cua-driver 0.28 answers `call check_permissions` with a JSON object
    // (`"accessibility": true, "screen_recording": true`,
    // `"direct_capture_status"`, `"screen_recording_capturable"`, plus the
    // daemon's own TCC identity); earlier builds printed
    // `✅ Accessibility: granted.` lines. Read the object when there is one —
    // the phrase match saw no "granted" in the JSON and kept the Settings row
    // on "needs access" with both grants in place — and keep the phrases for
    // the older driver.
    if let Some(value) = cua_permission_object(&report) {
        return CuaPermissionProbe {
            accessibility_granted: value.accessibility,
            screen_recording_granted: value.screen_recording,
            direct_capture_status: value.direct_capture_status,
            screen_recording_capturable: value.screen_recording_capturable,
            report,
        };
    }
    let normalized = report.to_ascii_lowercase();
    CuaPermissionProbe {
        accessibility_granted: normalized.contains("accessibility: granted"),
        screen_recording_granted: normalized.contains("screen recording: granted"),
        direct_capture_status: None,
        screen_recording_capturable: None,
        report,
    }
}

#[cfg(target_os = "macos")]
struct CuaPermissionObject {
    accessibility: bool,
    screen_recording: bool,
    direct_capture_status: Option<String>,
    screen_recording_capturable: Option<bool>,
}

/// The first JSON object in the driver's combined stdout/stderr, when it
/// carries both booleans. Log lines before or after the object are ignored.
#[cfg(target_os = "macos")]
fn cua_permission_object(report: &str) -> Option<CuaPermissionObject> {
    let start = report.find('{')?;
    let value = serde_json::Deserializer::from_str(&report[start..])
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()?;
    Some(CuaPermissionObject {
        accessibility: value.get("accessibility")?.as_bool()?,
        screen_recording: value.get("screen_recording")?.as_bool()?,
        direct_capture_status: value
            .get("direct_capture_status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        screen_recording_capturable: value
            .get("screen_recording_capturable")
            .and_then(serde_json::Value::as_bool),
    })
}

/// `cua-driver permissions grant` is the driver's documented grant path: it
/// launches CuaDriver.app through LaunchServices so macOS attributes the
/// dialogs to the app, requests Accessibility, Screen Recording and Tahoe's
/// direct-capture consent, then verifies a live capture. It waits for the
/// person, so it runs only from a user-initiated action.
#[cfg(target_os = "macos")]
async fn run_cua_permissions_grant(binary: &Path) -> Result<(), String> {
    let mut command = Command::new(binary);
    command
        .args(["permissions", "grant"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(300), command.output())
        .await
        .map_err(|_| "`cua-driver permissions grant` timed out after five minutes".to_string())?
        .map_err(|error| format!("failed to run {}: {error}", binary.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr: String = stderr.trim().chars().take(2048).collect();
    Err(format!(
        "`cua-driver permissions grant` exited with {}: {stderr}",
        output.status
    ))
}

#[cfg(not(target_os = "macos"))]
async fn probe_cua_desktop_observation(binary: &Path) -> Result<(bool, String), String> {
    let mut command = Command::new(binary);
    command
        .args(["call", "list_windows", "{}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "desktop observation timed out after 20 seconds".to_string())?
        .map_err(|error| format!("failed to run {}: {error}", binary.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(format!(
            "list_windows exited with {}: {}",
            output.status, stderr
        ));
    }
    let window_count = parse_cua_window_count(&stdout)?;
    let observed = window_count > 0;
    let report = format!(
        "Observed {} window(s).{}",
        window_count,
        if stderr.is_empty() {
            String::new()
        } else {
            format!(" {stderr}")
        }
    );
    Ok((observed, report))
}

#[cfg(any(not(target_os = "macos"), test))]
fn parse_cua_window_count(stdout: &str) -> Result<usize, String> {
    let body: serde_json::Value = serde_json::from_str(stdout)
        .map_err(|error| format!("list_windows returned invalid JSON: {error}"))?;
    let structured = body.get("structuredContent").unwrap_or(&body);
    structured
        .as_array()
        .or_else(|| {
            structured
                .get("windows")
                .and_then(serde_json::Value::as_array)
        })
        .map(Vec::len)
        .ok_or_else(|| "list_windows response did not contain a windows array".to_string())
}

pub(crate) async fn request_cua_driver_permission() -> Result<(), String> {
    let binary = runtime_core::cua::driver_binary().ok_or_else(|| {
        "CuaDriver is not installed. Use the setup guide to install it, then restart Magican Desktop."
            .to_string()
    })?;
    if !runtime_core::cua::has_desktop_session() {
        return Err(
            "No interactive desktop session is available. Start Magican Desktop as the signed-in desktop user."
                .to_string(),
        );
    }
    if !crate::host_gateway::ensure_cua_driver_daemon(&binary).await {
        return Err("Magican could not start the signed CuaDriver.app daemon.".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let probe = probe_cua_permissions(&binary).await?;
        if probe.granted() {
            return Ok(());
        }
        // Prefer the driver's own grant flow; open the first missing pane
        // only when it fails or leaves a grant missing.
        let grant_error = run_cua_permissions_grant(&binary).await.err();
        let probe = if grant_error.is_none() {
            let after = probe_cua_permissions(&binary).await?;
            if after.granted() {
                return Ok(());
            }
            after
        } else {
            probe
        };
        if let Some(error) = &grant_error {
            tracing::warn!("CuaDriver permission grant failed; opening System Settings: {error}");
        }
        let settings_url = if !probe.accessibility_granted {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        } else {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        };
        open::that(settings_url).map_err(|error| {
            format!(
                "CuaDriver still needs macOS access, and its settings pane could not be opened: {error}"
            )
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let (observed, report) = probe_cua_desktop_observation(&binary).await?;
        if observed {
            Ok(())
        } else {
            Err(format!(
                "CuaDriver is running but its desktop prerequisites are incomplete: {report}"
            ))
        }
    }
}

async fn request_speech_recognition_permission(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let config = app.state::<crate::AppState>().config.lock().await.clone();
        let helper = crate::host_gateway::resolve_speech_helper_binary(&config);
        let output = tokio::process::Command::new(&helper)
            .arg("authorize")
            .output()
            .await
            .map_err(|error| {
                format!(
                    "failed to run macOS Speech helper {}: {error}",
                    helper.display()
                )
            })?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Err(format!(
                "macOS Speech permission request failed status={} stderr={} stdout={}",
                output.status, stderr, stdout
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(())
    }
}

/// Magician's pinned CuaDriver setup (exact release, hash-checked installers,
/// `cua-driver permissions grant`), not the vendor's latest-release installer.
const CUA_SETUP_GUIDE_URL: &str =
    "https://github.com/MagicBeansAI/MagicianAI/blob/master/docs/components/scripts/cua-setup.md";

fn settings_url_for_permission(permission_key: &str) -> Option<&'static str> {
    match permission_key {
        "microphone" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        },
        "speech_recognition" => Some(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_SpeechRecognition",
        ),
        "input_monitoring" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        },
        "accessibility" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        },
        "full_disk_access" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        },
        "automation_messages" => {
            Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation")
        },
        "cua_driver" => Some(CUA_SETUP_GUIDE_URL),
        _ => None,
    }
}

fn build_macos_permission_statuses_from_inputs(
    inputs: PermissionStatusInputs,
) -> Vec<DesktopPermission> {
    vec![
        permission(
            "microphone",
            "Microphone",
            "Required for voice notes and live push-to-talk.",
            inputs.microphone,
            inputs.voice_enabled,
            "Open Microphone Settings",
            "Magican Desktop needs microphone access before host-native recording or live voice can capture audio.",
        ),
        permission(
            "input_monitoring",
            "Input Monitoring",
            "Required for gesture triggers and reliable Contextual Assist selection detection.",
            inputs.input_monitoring,
            inputs.gesture_enabled || inputs.contextual_assist_enabled,
            "Open Input Monitoring",
            "macOS blocks low-level keyboard and mouse-event listening until Magican Desktop is allowed here. Contextual Assist uses this as a fallback for browser/page selections that do not expose selected text through Accessibility.",
        ),
        permission(
            "accessibility",
            "Accessibility",
            "Required for Contextual Assist and reliable desktop presence.",
            inputs.accessibility,
            inputs.gesture_enabled || inputs.contextual_assist_enabled,
            "Open Accessibility",
            "This exact signed Magican Desktop build needs Accessibility to detect focused or selected text and receive the global gesture. An older Magican entry can remain visible in System Settings without granting the current build.",
        ),
        permission(
            "speech_recognition",
            "Speech Recognition",
            "Required when recorded STT uses Auto or macOS Speech.",
            inputs.speech_recognition,
            inputs.speech_required,
            "Open Speech Recognition",
            "Magican's local Speech helper performs on-device transcription under the signed desktop app's responsibility. Click Request Access first; macOS lists the current Magican build after it makes the request.",
        ),
        permission(
            "full_disk_access",
            "Full Disk Access",
            "Required to read your iMessage history.",
            inputs.full_disk_access,
            inputs.imessage_enabled,
            "Open Full Disk Access",
            "macOS requires Full Disk Access before Magican Desktop's background process can read the Messages database at ~/Library/Messages/chat.db. Grant it to Magican Desktop, then reopen it.",
        ),
        permission(
            "automation_messages",
            "Automation – Messages",
            "Required to send iMessages via the Messages app.",
            inputs.automation_messages,
            inputs.imessage_enabled,
            "Open Automation Settings",
            "macOS requires an Automation grant for Messages before Magican Desktop can send via Messages. The first send also triggers the system's Allow prompt.",
        ),
    ]
}

fn permission(
    key: &str,
    title: &str,
    description: &str,
    state: PermissionState,
    required: bool,
    settings_label: &str,
    details: &str,
) -> DesktopPermission {
    DesktopPermission {
        key: key.to_string(),
        title: title.to_string(),
        description: description.to_string(),
        state,
        required,
        settings_label: settings_label.to_string(),
        details: details.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_urls_cover_all_supported_permission_keys() {
        assert!(settings_url_for_permission("microphone")
            .unwrap()
            .contains("Privacy_Microphone"));
        assert!(settings_url_for_permission("speech_recognition")
            .unwrap()
            .contains("Privacy_SpeechRecognition"));
        assert!(settings_url_for_permission("input_monitoring")
            .unwrap()
            .contains("Privacy_ListenEvent"));
        assert!(settings_url_for_permission("accessibility")
            .unwrap()
            .contains("Privacy_Accessibility"));
        assert_eq!(
            settings_url_for_permission("cua_driver"),
            Some(CUA_SETUP_GUIDE_URL)
        );
        assert!(!CUA_SETUP_GUIDE_URL.contains("cua.ai"));
        assert!(settings_url_for_permission("unknown").is_none());
    }

    #[test]
    fn cua_permission_distinguishes_installation_from_an_interactive_session() {
        assert_eq!(
            cua_permission_status(true, true).state,
            PermissionState::Unknown
        );
        assert_eq!(
            cua_permission_status(true, false).state,
            PermissionState::Missing
        );
        assert_eq!(
            cua_permission_status(false, true).state,
            PermissionState::Missing
        );
        assert_eq!(
            cua_permission_status(true, true).settings_label,
            "Start & Verify"
        );
    }

    #[test]
    fn cua_desktop_observation_requires_a_real_window_array() {
        assert_eq!(
            parse_cua_window_count(r#"{"structuredContent":{"windows":[{"pid":42},{"pid":43}]}}"#)
                .unwrap(),
            2
        );
        assert_eq!(parse_cua_window_count(r#"{"windows":[]}"#).unwrap(), 0);
        assert!(parse_cua_window_count(r#"{"running":true}"#).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cua_permission_report_requires_both_desktop_grants() {
        let ready = parse_cua_permission_report(
            "✅ Accessibility: granted.\n✅ Screen Recording: granted.".to_string(),
        );
        assert!(ready.granted());

        let missing_capture = parse_cua_permission_report(
            "✅ Accessibility: granted.\n❌ Screen Recording: not granted.".to_string(),
        );
        assert!(!missing_capture.granted());
        assert!(missing_capture.accessibility_granted);
        assert!(!missing_capture.screen_recording_granted);
    }

    /// cua-driver 0.28.2's read-only `call check_permissions {}` — a JSON
    /// object with `direct_capture_status: "not_checked"`,
    /// `screen_recording_capturable: null` and the daemon's identity block,
    /// here behind a stderr log line.
    #[cfg(target_os = "macos")]
    #[test]
    fn cua_permission_report_reads_the_drivers_json_object() {
        let ready = parse_cua_permission_report(
            "cua-driver: probing TCC {\n  \"accessibility\": true,\n  \"direct_capture_error\": null,\n  \"direct_capture_status\": \"not_checked\",\n  \"screen_recording\": true,\n  \"screen_recording_capturable\": null,\n  \"source\": {\"attribution\": \"driver-daemon\", \"bundle_id\": \"com.trycua.driver\", \"pid\": 41645}\n}".to_string(),
        );
        assert!(ready.granted(), "{}", ready.report);
        assert_eq!(ready.direct_capture_status.as_deref(), Some("not_checked"));
        assert_eq!(ready.screen_recording_capturable, None);
        assert!(ready.direct_capture_detail().contains("permissions grant"));

        // A TCC grant whose live capture failed is not ready.
        let capture_failed = parse_cua_permission_report(
            "{\"accessibility\": true, \"screen_recording\": true, \"screen_recording_capturable\": false, \"direct_capture_status\": \"probe_failed\"}".to_string(),
        );
        assert!(!capture_failed.granted());
        let verified = parse_cua_permission_report(
            "{\"accessibility\": true, \"screen_recording\": true, \"screen_recording_capturable\": true, \"direct_capture_status\": \"ready\"}".to_string(),
        );
        assert!(verified.granted());
        assert_eq!(
            verified.direct_capture_detail(),
            "Direct screen capture is verified."
        );

        let missing_capture = parse_cua_permission_report(
            "{\"accessibility\": true, \"screen_recording\": false}".to_string(),
        );
        assert!(!missing_capture.granted());
        assert!(missing_capture.accessibility_granted);
        assert!(!missing_capture.screen_recording_granted);

        // An object without the booleans is not a verdict: fall back to the
        // phrases, which here say nothing — so nothing is granted.
        let unrelated =
            parse_cua_permission_report("{\"error\": \"daemon not ready\"}".to_string());
        assert!(!unrelated.accessibility_granted && !unrelated.screen_recording_granted);
    }

    #[test]
    fn macos_permission_rows_mark_only_relevant_items_required() {
        let rows = build_macos_permission_statuses_from_inputs(PermissionStatusInputs {
            voice_enabled: true,
            gesture_enabled: false,
            contextual_assist_enabled: false,
            speech_required: true,
            microphone: PermissionState::Granted,
            input_monitoring: PermissionState::Missing,
            accessibility: PermissionState::Unknown,
            speech_recognition: PermissionState::Missing,
            imessage_enabled: false,
            full_disk_access: PermissionState::Unknown,
            automation_messages: PermissionState::Unknown,
        });

        assert_eq!(rows.len(), 6);
        assert_required(&rows, "microphone", true);
        assert_required(&rows, "input_monitoring", false);
        assert_required(&rows, "accessibility", false);
        assert_required(&rows, "speech_recognition", true);
        assert_eq!(state_for(&rows, "microphone"), PermissionState::Granted);
        assert_eq!(
            state_for(&rows, "speech_recognition"),
            PermissionState::Missing
        );
    }

    #[test]
    fn macos_permission_rows_require_gesture_permissions_when_gestures_are_enabled() {
        let rows = build_macos_permission_statuses_from_inputs(PermissionStatusInputs {
            voice_enabled: false,
            gesture_enabled: true,
            contextual_assist_enabled: false,
            speech_required: false,
            microphone: PermissionState::Unknown,
            input_monitoring: PermissionState::Missing,
            accessibility: PermissionState::Granted,
            speech_recognition: PermissionState::Unknown,
            imessage_enabled: false,
            full_disk_access: PermissionState::Unknown,
            automation_messages: PermissionState::Unknown,
        });

        assert_required(&rows, "microphone", false);
        assert_required(&rows, "input_monitoring", true);
        assert_required(&rows, "accessibility", true);
        assert_required(&rows, "speech_recognition", false);
    }

    #[test]
    fn macos_permission_rows_require_accessibility_for_contextual_assist() {
        let rows = build_macos_permission_statuses_from_inputs(PermissionStatusInputs {
            voice_enabled: false,
            gesture_enabled: false,
            contextual_assist_enabled: true,
            speech_required: false,
            microphone: PermissionState::Unknown,
            input_monitoring: PermissionState::Unknown,
            accessibility: PermissionState::Missing,
            speech_recognition: PermissionState::Unknown,
            imessage_enabled: false,
            full_disk_access: PermissionState::Unknown,
            automation_messages: PermissionState::Unknown,
        });

        assert_required(&rows, "accessibility", true);
        assert_required(&rows, "input_monitoring", true);
    }

    fn inputs_with(imessage_enabled: bool) -> PermissionStatusInputs {
        PermissionStatusInputs {
            voice_enabled: true,
            gesture_enabled: true,
            contextual_assist_enabled: true,
            speech_required: true,
            microphone: PermissionState::Granted,
            input_monitoring: PermissionState::Granted,
            accessibility: PermissionState::Granted,
            speech_recognition: PermissionState::Granted,
            imessage_enabled,
            full_disk_access: PermissionState::Granted,
            automation_messages: PermissionState::Granted,
        }
    }

    #[test]
    fn settings_urls_cover_imessage_permission_keys() {
        assert!(settings_url_for_permission("full_disk_access")
            .unwrap()
            .contains("Privacy_AllFiles"));
        assert!(settings_url_for_permission("automation_messages")
            .unwrap()
            .contains("Privacy_Automation"));
    }

    #[test]
    fn imessage_rows_required_only_when_imessage_enabled() {
        let on = build_macos_permission_statuses_from_inputs(inputs_with(true));
        assert_required(&on, "full_disk_access", true);
        assert_required(&on, "automation_messages", true);
        let off = build_macos_permission_statuses_from_inputs(inputs_with(false));
        assert_required(&off, "full_disk_access", false);
        assert_required(&off, "automation_messages", false);
    }

    fn assert_required(rows: &[DesktopPermission], key: &str, expected: bool) {
        assert_eq!(
            rows.iter()
                .find(|row| row.key == key)
                .unwrap_or_else(|| panic!("missing row {key}"))
                .required,
            expected,
            "required flag for {key}"
        );
    }

    fn state_for(rows: &[DesktopPermission], key: &str) -> PermissionState {
        rows.iter()
            .find(|row| row.key == key)
            .unwrap_or_else(|| panic!("missing row {key}"))
            .state
            .clone()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        build_macos_permission_statuses_from_inputs, DesktopPermission, PermissionState,
        PermissionStatusInputs,
    };
    use crate::voice_gesture;
    use block2::RcBlock;
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        CallbackResult,
    };
    use objc2::{class, msg_send, runtime::Bool};
    use objc2_foundation::NSString;
    use std::{
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tauri::{AppHandle, Manager};
    use tokio::{sync::oneshot, time::timeout};

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    pub(super) async fn build_permission_statuses(app: &AppHandle) -> Vec<DesktopPermission> {
        let config = app
            .state::<crate::AppState>()
            .config
            .try_lock()
            .ok()
            .map(|config| config.clone());
        let voice_enabled = true;
        let gesture_enabled = config
            .as_ref()
            .map(|config| {
                voice_gesture::overlay_gesture_label(&config.general.quick_overlay_gesture)
                    .is_some()
            })
            .unwrap_or(true);
        // Provider selection is backend-owned and may fall back to macOS
        // Speech, so the host advertises the permission whenever voice capture
        // is available instead of mirroring provider inventory in desktop TOML.
        let speech_required = voice_enabled;
        let contextual_assist_enabled = config
            .as_ref()
            .map(|config| config.contextual_assist.enabled)
            .unwrap_or(true);

        build_macos_permission_statuses_from_inputs(PermissionStatusInputs {
            voice_enabled,
            gesture_enabled,
            contextual_assist_enabled,
            speech_required,
            microphone: microphone_state(),
            input_monitoring: input_monitoring_state(),
            accessibility: accessibility_state(),
            speech_recognition: speech_recognition_state(app).await,
            imessage_enabled: true,
            full_disk_access: full_disk_access_state(),
            automation_messages: automation_messages_state().await,
        })
    }

    fn accessibility_state() -> PermissionState {
        if unsafe { AXIsProcessTrusted() } != 0 {
            PermissionState::Granted
        } else {
            PermissionState::Missing
        }
    }

    fn input_monitoring_state() -> PermissionState {
        let result = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            vec![CGEventType::FlagsChanged],
            |_proxy, _event_type, _event| CallbackResult::Keep,
        );
        if result.is_ok() {
            PermissionState::Granted
        } else {
            PermissionState::Missing
        }
    }

    fn microphone_state() -> PermissionState {
        match av_authorization_status("soun") {
            Some(3) => PermissionState::Granted,
            Some(1 | 2) => PermissionState::Missing,
            Some(0) => PermissionState::Unknown,
            _ => PermissionState::Unknown,
        }
    }

    pub(super) async fn request_microphone_permission() -> Result<(), String> {
        match microphone_state() {
            PermissionState::Granted => return Ok(()),
            PermissionState::Missing => {
                return Err(
                    "Microphone access was denied. Enable Magican in macOS Microphone settings."
                        .to_string(),
                )
            },
            PermissionState::Unknown | PermissionState::Unsupported => {},
        }

        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        start_microphone_permission_request(sender);

        match timeout(Duration::from_secs(60), receiver).await {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(
                "Microphone access was not granted. Enable Magican in macOS Microphone settings."
                    .to_string(),
            ),
            Ok(Err(_)) => Err("macOS closed the Microphone permission request.".to_string()),
            Err(_) => {
                Err("Timed out waiting for the macOS Microphone permission response.".to_string())
            },
        }
    }

    fn start_microphone_permission_request(sender: Arc<Mutex<Option<oneshot::Sender<bool>>>>) {
        let completion_sender = Arc::clone(&sender);
        let completion: RcBlock<dyn Fn(Bool)> = RcBlock::new(move |granted: Bool| {
            if let Ok(mut sender) = completion_sender.lock() {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(granted.as_bool());
                }
            }
        });
        let media_type = NSString::from_str("soun");
        unsafe {
            let _: () = msg_send![class!(AVCaptureDevice),
                requestAccessForMediaType: &*media_type,
                completionHandler: &*completion
            ];
        }
    }

    async fn speech_recognition_state(app: &AppHandle) -> PermissionState {
        let config = app.state::<crate::AppState>().config.lock().await.clone();
        let helper = crate::host_gateway::resolve_speech_helper_binary(&config);
        let output = match tokio::process::Command::new(&helper)
            .arg("status")
            .output()
            .await
        {
            Ok(output) => output,
            Err(_) => return PermissionState::Unknown,
        };
        if !output.status.success() {
            return PermissionState::Unknown;
        }
        let parsed: super::SpeechHelperAuthorizationOutput =
            match serde_json::from_slice(&output.stdout) {
                Ok(parsed) => parsed,
                Err(_) => return PermissionState::Unknown,
            };
        match parsed.status.as_str() {
            "authorized" => PermissionState::Granted,
            "denied" | "restricted" => PermissionState::Missing,
            "not_determined" => PermissionState::Unknown,
            _ => PermissionState::Unknown,
        }
    }

    fn av_authorization_status(media_type: &str) -> Option<isize> {
        let class = class!(AVCaptureDevice);
        let media_type = NSString::from_str(media_type);
        let status: isize =
            unsafe { msg_send![class, authorizationStatusForMediaType: &*media_type] };
        Some(status)
    }

    fn full_disk_access_state() -> PermissionState {
        let path = std::env::var("MAGICIAN_IMESSAGE_DB_PATH")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join("Library/Messages/chat.db")
            });
        // A read attempt is the reliable probe: EACCES => FDA missing.
        match std::fs::File::open(&path) {
            Ok(_) => PermissionState::Granted,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => PermissionState::Missing,
            Err(_) => PermissionState::Unknown, // Messages not set up / path absent
        }
    }

    async fn automation_messages_state() -> PermissionState {
        // A harmless read of Messages. Exit 0 => authorized; the standard "not authorized"
        // (errAEEventNotPermitted / -1743) => missing. NOTE: on the FIRST run macOS shows the
        // Allow prompt — intentional, that's the grant path.
        let out = tokio::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(r#"tell application "Messages" to get name"#)
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => PermissionState::Granted,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr).to_lowercase();
                if err.contains("not authorized") || err.contains("-1743") {
                    PermissionState::Missing
                } else {
                    PermissionState::Unknown
                }
            },
            Err(_) => PermissionState::Unknown,
        }
    }
}
