//! Server-side screen capture: the `screencapture` binary wrapper, capture
//! targets, attachment-context staging, and session-staged full-screen grabs
//! for voice guided flows. Extracted from `api::screen_api` (api-crate
//! extraction prerequisite); the api file re-imports for its handlers.

use serde::Deserialize;
use tokio::process::Command;

use magician::magician_v2::chat::models::{
    ScreenCaptureAttachmentContext, ScreenCaptureImageSize, ScreenCaptureScreenRect,
};
use magician::magician_v2::chat::service::{
    ChatService, SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE,
    SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL,
};

/// macOS built-in capture CLI. Absolute path — the server may run under a
/// minimal PATH. Screen-recording TCC attributes to the process that launched
/// magician (terminal in dev); a blank/wallpaper-only capture means the grant
/// is missing there.
pub const SCREENCAPTURE_BIN: &str = "/usr/sbin/screencapture";

#[derive(Debug, Clone, Deserialize)]
pub struct ScreenCaptureRequestRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub fn screen_capture_attachment_context(
    mode: &str,
    region_rect: Option<&ScreenCaptureRequestRect>,
    mime_type: &str,
    bytes: &[u8],
) -> Option<ScreenCaptureAttachmentContext> {
    if !mime_type.starts_with("image/") {
        return None;
    }
    let image_size =
        png_image_size(bytes).map(|(width, height)| ScreenCaptureImageSize { width, height });
    match mode {
        "region" => {
            if let Some(rect) = region_rect {
                return Some(ScreenCaptureAttachmentContext {
                    server_registered: true,
                    mode: mode.to_string(),
                    coordinate_space: SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE.to_string(),
                    image_size,
                    screen_rect: Some(ScreenCaptureScreenRect {
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    }),
                });
            }
            Some(ScreenCaptureAttachmentContext {
                server_registered: true,
                mode: mode.to_string(),
                coordinate_space: SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL.to_string(),
                image_size,
                screen_rect: None,
            })
        },
        "screenshot" => image_size.map(|image_size| ScreenCaptureAttachmentContext {
            server_registered: true,
            mode: mode.to_string(),
            coordinate_space: SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE.to_string(),
            // `/usr/sbin/screencapture` without an explicit region captures
            // the main display. Its PNG dimensions are physical pixels, the
            // same coordinate unit Tauri exposes for monitor bounds. Persist
            // that rect so the desktop overlay can deterministically map
            // image-local model output instead of treating a full screenshot
            // like an unknown-origin interactive crop.
            screen_rect: Some(ScreenCaptureScreenRect {
                x: 0,
                y: 0,
                width: image_size.width,
                height: image_size.height,
            }),
            image_size: Some(image_size),
        }),
        _ => None,
    }
}

/// Capture and stage a fresh server-attested full-screen image directly onto
/// an existing chat session. Realtime voice uses this before entering a
/// screen-bound guided flow so the protected lane never relies on a
/// client-authored screenshot.
pub async fn capture_full_screen_attachment_for_session(
    chat_service: &ChatService,
    session_id: &str,
    feature_surface: &str,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<String, String> {
    let capture = capture_display(ScreenCaptureTarget::Full);
    let bytes = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            return Err("voice guided-flow screen capture was cancelled".to_string());
        },
        result = capture => result,
    }?
    .ok_or_else(|| "full-screen capture was cancelled".to_string())?;
    if cancellation.is_cancelled() {
        return Err("voice guided-flow screen capture was cancelled".to_string());
    }
    let context = screen_capture_attachment_context("screenshot", None, "image/png", &bytes)
        .ok_or_else(|| "captured screen did not produce valid PNG dimensions".to_string())?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let (filename_prefix, description) = match feature_surface {
        "tutor" => (
            "voice-tutor-screen",
            "fresh voice Personal Tutor screen capture",
        ),
        "app_copilot" => (
            "voice-app-copilot-screen",
            "fresh voice App Copilot screen capture",
        ),
        _ => (
            "voice-guided-flow-screen",
            "fresh voice guided-flow screen capture",
        ),
    };
    let record = chat_service
        .store_attachment_with_context(
            session_id,
            &format!("{filename_prefix}-{stamp}.png"),
            "image/png",
            &bytes,
            Some(description.to_string()),
            Some(context),
        )
        .await
        .map_err(|error| format!("stage voice guided-flow capture: {error}"))?;
    Ok(record.id)
}

pub fn png_image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 24 || &bytes[..8] != PNG_SIGNATURE {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// How long the interactive picker may sit on screen before we give up
/// (the user wandered off mid-selection).
pub const REGION_PICK_TIMEOUT_SECS: u64 = 90;

#[derive(Debug, Clone)]
pub enum ScreenCaptureTarget {
    Full,
    InteractiveRegion,
    Rect(ScreenCaptureRequestRect),
}

/// Capture PNG bytes via the macOS built-in `screencapture` (silent: no
/// shutter sound). `InteractiveRegion` adds `-i` — the native crosshair picker
/// (drag = region, spacebar = window) — and `Ok(None)` then means the user
/// cancelled the selection (Escape / timeout / nothing written). `Rect` uses
/// `-R` so callers that know the global screen rect get a crop with mappable
/// overlay coordinates.
pub async fn capture_display(target: ScreenCaptureTarget) -> Result<Option<Vec<u8>>, String> {
    let tmp = std::env::temp_dir().join(format!(
        "magician-screen-{}.png",
        uuid::Uuid::new_v4().simple()
    ));
    let mut command = Command::new(SCREENCAPTURE_BIN);
    let interactive = matches!(target, ScreenCaptureTarget::InteractiveRegion);
    match target {
        ScreenCaptureTarget::Full => {},
        ScreenCaptureTarget::InteractiveRegion => {
            command.arg("-i");
        },
        ScreenCaptureTarget::Rect(rect) => {
            command.arg(format!(
                "-R{},{},{},{}",
                rect.x, rect.y, rect.width, rect.height
            ));
        },
    }
    command.arg("-x").arg("-t").arg("png").arg(&tmp);
    // kill_on_drop: an abandoned picker must not outlive the request — the
    // timeout below drops the future, which dismisses the selection UI.
    command.kill_on_drop(true);
    let run = command.output();
    let output = if interactive {
        match tokio::time::timeout(
            std::time::Duration::from_secs(REGION_PICK_TIMEOUT_SECS),
            run,
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Ok(None); // selection timed out — treat as cancel
            },
        }
    } else {
        run.await
    }
    .map_err(|e| format!("spawn {SCREENCAPTURE_BIN}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = tokio::fs::remove_file(&tmp).await;
        if interactive {
            // Escape from the picker exits non-zero with nothing written —
            // a cancel, not a failure.
            tracing::debug!(
                target: "screen_capture",
                status = %output.status,
                stderr = %stderr.trim(),
                "interactive screencapture ended without a capture"
            );
            return Ok(None);
        }
        return Err(format!(
            "screencapture exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    let bytes = match tokio::fs::read(&tmp).await {
        Ok(bytes) => bytes,
        Err(error) if interactive => {
            // Some macOS builds exit 0 on Escape and just write no file.
            tracing::debug!(target: "screen_capture", %error, "no file after interactive pick — cancelled");
            return Ok(None);
        },
        Err(error) => return Err(format!("read capture file: {error}")),
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    if bytes.is_empty() {
        if interactive {
            return Ok(None);
        }
        return Err("screencapture produced an empty file".to_string());
    }
    Ok(Some(bytes))
}
