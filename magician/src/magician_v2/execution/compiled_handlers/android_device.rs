//! The verbs an agent uses to work an Android phone.
//!
//! The companion exposes thirty-three device actions. Surfacing all of them
//! would repeat the mistake `agent-browser` avoided: an agent given a catalogue
//! spends its attention choosing between near-identical tools instead of doing
//! the task. So the acting roster is four verbs, and the companion's action
//! names become the vocabulary *inside* `android_act`.
//!
//! The companion's full roster remains behind these four verbs as authoritative
//! MCP `tools/list` state. It is transport authority, not another model-facing
//! verb.
//!
//! Sight is structural first. `android_snapshot` returns the screen as
//! addressable rows and is what an agent should read; `android_screenshot`
//! exists for when the structure is not enough — an unlabelled canvas, a chart,
//! a captcha. Measured on a real handset, most native apps name 78–100% of their
//! controls, so pixels are the exception rather than the default.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::magician_v2::device_bridge::{
    global_hub, DeviceBridgeError, DeviceBridgeHub, DeviceKey, DEFAULT_DEVICE_ACTION_TIMEOUT,
};
use crate::magician_v2::device_governance::{
    global_device_audit, global_device_policy, DeviceActionRecord, DeviceActionVerdict,
    ScreenshotPolicy,
};
use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

use super::shared::require_scope_str;

/// Resolve which phone a call is for.
///
/// With one device connected the agent should not have to name it — asking for
/// an id the owner has never seen is friction for nothing. With several, the
/// call is refused rather than guessed, because acting on the wrong phone is
/// not recoverable by retrying.
async fn resolve_device(
    hub: &DeviceBridgeHub,
    principal: &str,
    workspace: &str,
    requested: Option<&str>,
) -> Result<DeviceKey, Value> {
    let connected = hub
        .connected_devices()
        .into_iter()
        .filter(|key| key.principal == principal && key.workspace == workspace)
        .collect::<Vec<_>>();

    if let Some(device_id) = requested {
        return connected
            .into_iter()
            .find(|key| key.device_id == device_id)
            .ok_or_else(|| {
                error_value(
                    "device_not_connected",
                    &format!("no device `{device_id}` is connected for this scope"),
                    false,
                )
            });
    }

    match connected.len() {
        0 => Err(error_value(
            "no_device_connected",
            "no Android companion is connected. Open Magdroid on the phone and check it is paired.",
            true,
        )),
        1 => Ok(connected.into_iter().next().expect("length checked")),
        _ => {
            let ids = connected
                .iter()
                .map(|key| key.device_id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(error_value(
                "multiple_devices_connected",
                &format!("several devices are connected; name one with `device`: {ids}"),
                false,
            ))
        },
    }
}

fn error_value(reason: &str, detail: &str, retryable: bool) -> Value {
    json!({
        "status": "error",
        "reason": reason,
        "detail": detail,
        "retryable": retryable,
    })
}

/// Map a bridge failure onto something an agent can act on.
///
/// The distinction that matters is retryable versus not: a phone that was
/// asleep is worth one more try, a rejected action is not, and an agent that
/// cannot tell them apart either gives up too early or loops.
fn bridge_error(error: DeviceBridgeError) -> Value {
    let (reason, retryable) = match &error {
        DeviceBridgeError::NotConnected(_) => ("no_device_connected", true),
        DeviceBridgeError::SessionUnavailable(_) => ("device_session_unavailable", true),
        DeviceBridgeError::Timeout(_) => ("device_timeout", true),
        DeviceBridgeError::Disconnected => ("device_disconnected", true),
        DeviceBridgeError::DeviceError(_) => ("device_refused_action", false),
        DeviceBridgeError::AppProtected(_) => ("app_protected", false),
        DeviceBridgeError::EmptyResult => ("device_returned_nothing", false),
        DeviceBridgeError::ConnectionChanged => ("device_connection_changed", true),
        DeviceBridgeError::ResultTooLarge(_) => ("device_result_too_large", false),
    };
    error_value(reason, &error.to_string(), retryable)
}

async fn dispatch(
    args: &Value,
    tool: &str,
    action: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, ExecutionError> {
    let principal = require_scope_str(args, "__principal", tool)?;
    let workspace = require_scope_str(args, "__workspace", tool)?;

    let Some(hub) = global_hub() else {
        audit(
            &principal,
            &workspace,
            "",
            tool,
            action,
            None,
            DeviceActionVerdict::Error,
            Some("device_bridge_unavailable".to_string()),
        )
        .await;
        return Ok(error_value(
            "device_bridge_unavailable",
            "the device bridge is not running in this process",
            false,
        ));
    };
    let requested = args.get("device").and_then(Value::as_str);
    let key = match resolve_device(&hub, &principal, &workspace, requested).await {
        Ok(key) => key,
        Err(problem) => {
            // A refusal is still a device action the owner would want
            // recounted — an agent probing for devices, or acting when none
            // is paired, is exactly the activity an audit exists to see.
            // The first cut skipped these and its own changelog overclaimed
            // "every refusal"; now it is true.
            let detail = problem
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("device_not_resolved")
                .to_string();
            audit(
                &principal,
                &workspace,
                "",
                tool,
                action,
                None,
                DeviceActionVerdict::Error,
                Some(detail),
            )
            .await;
            return Ok(problem);
        },
    };

    match hub.dispatch(&key, action, params, timeout).await {
        Ok(result) => {
            // The device stamps the foreground package (and, on a protection
            // refusal, `app_protected`) into `structuredContent`; the bridge
            // projects it through verbatim. This is where "what was done to
            // which app" becomes a durable record.
            let structured = result.get("structuredContent");
            let foreground = structured
                .and_then(|content| content.get("foreground_package"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let refused_protected = structured
                .and_then(|content| content.get("app_protected"))
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let verdict = if refused_protected {
                DeviceActionVerdict::AppProtected
            } else {
                DeviceActionVerdict::Ok
            };
            audit(
                &principal,
                &workspace,
                &key.device_id,
                tool,
                action,
                foreground.clone(),
                verdict,
                None,
            )
            .await;

            if refused_protected {
                let package = foreground.as_deref().unwrap_or("the foreground app");
                return Ok(error_value(
                    "app_protected",
                    &format!(
                        "{package} is protected on this device; its owner controls the list \
                         under Settings > Protected apps on the phone"
                    ),
                    false,
                ));
            }
            Ok(json!({
                "status": "ok",
                "device": key.device_id,
                "result": result,
            }))
        },
        Err(error) => {
            let detail = error.to_string();
            let (foreground, verdict) = match &error {
                DeviceBridgeError::AppProtected(package) => {
                    (Some(package.clone()), DeviceActionVerdict::AppProtected)
                },
                _ => (None, DeviceActionVerdict::Error),
            };
            audit(
                &principal,
                &workspace,
                &key.device_id,
                tool,
                action,
                foreground,
                verdict,
                Some(detail),
            )
            .await;
            Ok(bridge_error(error))
        },
    }
}

/// Best-effort by design: a device action must not fail because its audit
/// line could not be written, but a failed write is still said out loud —
/// an audit that silently misses records is worse than none, because it
/// gets believed.
#[allow(clippy::too_many_arguments)]
async fn audit(
    principal: &str,
    workspace: &str,
    device_id: &str,
    tool: &str,
    action: &str,
    foreground_package: Option<String>,
    verdict: DeviceActionVerdict,
    detail: Option<String>,
) {
    let Some(audit) = global_device_audit() else {
        return;
    };
    let record = DeviceActionRecord {
        ts_ms: chrono::Utc::now().timestamp_millis(),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        device_id: device_id.to_string(),
        tool: tool.to_string(),
        action: action.to_string(),
        foreground_package,
        verdict,
        detail,
        screenshot: tool == "android_screenshot" && verdict == DeviceActionVerdict::Ok,
        connection_generation: None,
        play_integrity_verdict_digest: None,
    };
    if let Err(error) = audit.append(record).await {
        tracing::warn!(%error, tool, "device action ran but its audit record did not persist");
    }
}

fn text_arg(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// How many times a snapshot asks again when the phone has no active
/// window, and how long it waits between asks. The accessibility service's
/// `rootInActiveWindow` is null while one app's window hands over to
/// another's — the observation the loop takes right after `android_app
/// launch` lands in exactly that gap on a cold start — and while the screen
/// is off. Three asks over ~2 s cover the hand-over; a dark screen is still
/// dark after them and is reported as such.
const NO_ACTIVE_WINDOW_RETRIES: usize = 3;
const NO_ACTIVE_WINDOW_RETRY_DELAY: Duration = Duration::from_millis(750);

/// The device's "No active window available": a transient, not a refusal.
/// The bridge maps every device-side error to `device_refused_action`
/// (not retryable), which told the first Apple Music run that its
/// post-launch snapshot was final; the agent yielded on iteration 4.
fn is_no_active_window(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("error")
        && value
            .get("detail")
            .and_then(Value::as_str)
            .is_some_and(|detail| detail.contains("No active window"))
}

/// `android_snapshot` — the screen as structure. Primary sight.
pub async fn snapshot(
    _resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let mut params = json!({
        // Interactive by default: an agent acts on controls, and the full tree
        // is mostly layout containers it can do nothing with.
        "filter": text_arg(&args, "filter").unwrap_or_else(|| "interactive".to_string()),
    });
    if let Some(depth) = args.get("max_depth").and_then(Value::as_u64) {
        params["max_depth"] = json!(depth);
    }
    for attempt in 1..=NO_ACTIVE_WINDOW_RETRIES {
        let result = dispatch(
            &args,
            "android_snapshot",
            "android_get_ui_tree",
            params.clone(),
            DEFAULT_DEVICE_ACTION_TIMEOUT,
        )
        .await?;
        if !is_no_active_window(&result) {
            return Ok(result);
        }
        if attempt < NO_ACTIVE_WINDOW_RETRIES {
            tracing::info!(
                attempt,
                "[ANDROID-SNAPSHOT] the phone has no active window yet; asking again"
            );
            tokio::time::sleep(NO_ACTIVE_WINDOW_RETRY_DELAY).await;
        }
    }
    Ok(error_value(
        "no_active_window",
        &format!(
            "the phone reported no active window {NO_ACTIVE_WINDOW_RETRIES} times over ~{} s: \
             it is either still switching apps or its screen is off or locked. Wake and unlock \
             the phone (or ask the owner to), then snapshot again.",
            (NO_ACTIVE_WINDOW_RETRY_DELAY.as_millis() as usize * (NO_ACTIVE_WINDOW_RETRIES - 1))
                / 1000
        ),
        true,
    ))
}

/// `android_act` — one action against the screen.
pub async fn act(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(action) = text_arg(&args, "action") else {
        return Ok(error_value(
            "invalid_request",
            "android_act requires `action`, for example tap, type, swipe, key or back",
            false,
        ));
    };

    // The agent names an intent; the companion's own action vocabulary stays an
    // implementation detail rather than thirty-three entries on the roster.
    let (device_action, params) = match action.as_str() {
        "tap" => (
            "android_tap",
            json!({ "x": args.get("x"), "y": args.get("y") }),
        ),
        "long_press" => (
            "android_long_press",
            json!({ "x": args.get("x"), "y": args.get("y") }),
        ),
        "type" => (
            "android_input_text",
            // No element selector on purpose: the companion types into whatever
            // holds focus, which is the only thing that reaches an empty,
            // unlabelled field — a phone number or OTP box.
            json!({ "text": text_arg(&args, "text").unwrap_or_default() }),
        ),
        "swipe" => (
            "android_swipe",
            json!({
                "start_x": args.get("x"), "start_y": args.get("y"),
                "end_x": args.get("to_x"), "end_y": args.get("to_y"),
            }),
        ),
        "key" | "back" | "home" | "enter" => (
            "android_press_key",
            json!({ "key": if action == "key" {
                text_arg(&args, "key").unwrap_or_else(|| "back".to_string())
            } else { action.clone() } }),
        ),
        "double_tap" => (
            "android_double_tap",
            json!({ "x": args.get("x"), "y": args.get("y") }),
        ),
        "drag" => (
            "android_drag",
            json!({
                "from_x": args.get("x"), "from_y": args.get("y"),
                "to_x": args.get("to_x"), "to_y": args.get("to_y"),
            }),
        ),
        "pinch" => (
            "android_pinch",
            json!({ "center_x": args.get("x"), "center_y": args.get("y"), "scale": args.get("scale") }),
        ),
        "open_url" => (
            "android_open_url",
            json!({ "url": text_arg(&args, "url").unwrap_or_default() }),
        ),
        "set_clipboard" => (
            "android_set_clipboard",
            json!({ "text": text_arg(&args, "text").unwrap_or_default() }),
        ),
        "system" => (
            "android_global_action",
            json!({ "action": text_arg(&args, "name").unwrap_or_default() }),
        ),
        "wait_for" | "wait_gone" | "scroll_to" => {
            let mut target = serde_json::Map::new();
            for field in ["text", "resource_id", "content_desc", "timeout_ms"] {
                if let Some(value) = args.get(field).filter(|value| !value.is_null()) {
                    target.insert(field.to_string(), value.clone());
                }
            }
            if action == "scroll_to" {
                for field in ["direction", "max_scrolls"] {
                    if let Some(value) = args.get(field).filter(|value| !value.is_null()) {
                        target.insert(field.to_string(), value.clone());
                    }
                }
            }
            let device_action = match action.as_str() {
                "wait_for" => "android_wait_for_element",
                "wait_gone" => "android_wait_for_gone",
                _ => "android_scroll_to_element",
            };
            (device_action, Value::Object(target))
        },
        "wait_idle" => (
            "android_wait_for_idle",
            json!({ "timeout_ms": args.get("timeout_ms") }),
        ),
        other => {
            return Ok(error_value(
                "unknown_action",
                &format!(
                "`{other}` is not an action. Use tap, long_press, double_tap, type, swipe, drag, \
                     pinch, key, back, home, enter, open_url, set_clipboard, system, wait_for, \
                     wait_gone, wait_idle or scroll_to."
            ),
                false,
            ))
        },
    };

    dispatch(
        &args,
        "android_act",
        device_action,
        params,
        DEFAULT_DEVICE_ACTION_TIMEOUT,
    )
    .await
}

/// `android_screenshot` — pixels, when structure is not enough.
pub async fn screenshot(
    resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    // The scope-wide switch, checked before any device is asked. The device's
    // own per-app gate refuses protected apps regardless of this answer.
    if let Some(policy) = global_device_policy() {
        if policy.screenshot_policy().await == ScreenshotPolicy::BlockAll {
            let principal = require_scope_str(&args, "__principal", "android_screenshot")?;
            let workspace = require_scope_str(&args, "__workspace", "android_screenshot")?;
            audit(
                &principal,
                &workspace,
                "",
                "android_screenshot",
                "android_screenshot",
                None,
                DeviceActionVerdict::PolicyBlocked,
                None,
            )
            .await;
            return Ok(error_value(
                "screenshot_policy_blocked",
                "device screenshots are disabled for this deployment; the owner can \
                 re-enable them via the device policy API",
                false,
            ));
        }
    }
    let mut envelope = dispatch(
        &args,
        "android_screenshot",
        "android_screenshot",
        json!({}),
        DEFAULT_DEVICE_ACTION_TIMEOUT,
    )
    .await?;
    if let Some(file) = detach_screenshot_to_file(&resources, &args, &mut envelope).await {
        envelope["screenshot_file"] = json!(file.path.to_string_lossy());
        envelope["screenshot_media_type"] = json!(file.media_type);
    }
    Ok(envelope)
}

/// Where a device screenshot lands and what it is.
struct SavedScreenshot {
    path: std::path::PathBuf,
    media_type: String,
}

/// Move the JPEG out of the result envelope and onto disk, under the scope's
/// `apps/captures/android/<execution>/`.
///
/// The device answers with the image as an MCP `image` block: ~55 KB of
/// base64 for a 720×1520 phone. Left in the result it did two wrong things.
/// It pushed the result past the inline threshold, so the loop recorded an
/// omission stub and the decision seam — which attaches the last action's
/// capture to the model as an image — could not find the block; and every
/// Android decision so far read `has_images=false`, the model photographed
/// `44` and cleared it, and a sign-in sheet it had photographed twice was
/// "not yielding readable sheet content". And what the model was given
/// instead was the stub: a screenshot it was told about and never saw.
/// With the bytes on disk the envelope is a few hundred bytes of metadata
/// plus `screenshot_file`, the decision attaches the file, and the durable
/// record is a JPEG in a captures directory rather than base64 in a JSON
/// output. Best effort: a save that fails leaves the block in place.
async fn detach_screenshot_to_file(
    resources: &AgentResources,
    args: &Value,
    envelope: &mut Value,
) -> Option<SavedScreenshot> {
    let device = envelope
        .get("device")
        .and_then(Value::as_str)
        .unwrap_or("device")
        .to_string();
    let content = envelope
        .get_mut("result")?
        .get_mut("content")?
        .as_array_mut()?;
    let index = content
        .iter()
        .position(|block| block.get("type").and_then(Value::as_str) == Some("image"))?;
    let media_type = content[index]
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("image/jpeg")
        .to_string();
    let data = content[index].get("data").and_then(Value::as_str)?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    if bytes.is_empty() {
        return None;
    }
    let principal = args.get("__principal").and_then(Value::as_str)?;
    let workspace = args.get("__workspace").and_then(Value::as_str)?;
    let execution = args
        .get("__execution_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or("adhoc");
    let extension = match media_type.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        _ => "jpg",
    };
    let path = resources
        .artifact_workspace
        .app_captures_root(principal, workspace)
        .join("android")
        .join(execution)
        .join(format!(
            "{}-{device}.{extension}",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ")
        ));
    if let Err(error) = resources
        .artifact_workspace
        .write_path_raw(&path, &bytes)
        .await
    {
        tracing::warn!(
            %error,
            path = %path.display(),
            "[ANDROID-SCREENSHOT] the capture could not be saved; the image stays in the result"
        );
        return None;
    }
    content.remove(index);
    Some(SavedScreenshot { path, media_type })
}

/// `android_notifications` — what is waiting in the phone's notification
/// shade.
///
/// The companion filters this read rather than this runtime: a notification it
/// judges to be verification material comes back as a withheld marker and is
/// counted in `withheld_count`, never as digits (secure HITL P6). A code
/// reaches a run through the custody lane alone, so a tool result — and the
/// model behind it — never carries one. The marker is deliberately visible:
/// told that something is being withheld and how much, an agent asks the
/// person, where silence sends it looking for the same text through the
/// screen instead.
pub async fn notifications(
    _resources: Arc<AgentResources>,
    args: Value,
) -> Result<Value, ExecutionError> {
    let mut params = json!({});
    if let Some(active_only) = args.get("active_only").and_then(Value::as_bool) {
        params["active_only"] = json!(active_only);
    }
    dispatch(
        &args,
        "android_notifications",
        "android_get_notifications",
        params,
        DEFAULT_DEVICE_ACTION_TIMEOUT,
    )
    .await
}

/// `android_app` — launch, close, or list what is installed.
pub async fn app(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let operation = text_arg(&args, "operation").unwrap_or_else(|| "launch".to_string());
    let package = text_arg(&args, "package");

    let (device_action, params) = match operation.as_str() {
        "list" => ("android_list_apps", json!({})),
        "launch" | "close" => {
            let Some(package) = package else {
                return Ok(error_value(
                    "invalid_request",
                    &format!("`{operation}` needs a `package`, for example com.example.app"),
                    false,
                ));
            };
            let action = if operation == "launch" {
                "android_launch_app"
            } else {
                "android_close_app"
            };
            (action, json!({ "package_name": package }))
        },
        other => {
            return Ok(error_value(
                "unknown_operation",
                &format!("`{other}` is not an operation. Use launch, close or list."),
                false,
            ))
        },
    };

    // Launching an app can wait on a cold start, so this is given the same
    // generous budget rather than a tighter one.
    dispatch(
        &args,
        "android_app",
        device_action,
        params,
        DEFAULT_DEVICE_ACTION_TIMEOUT,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_window_is_the_only_device_error_a_snapshot_asks_again_for() {
        assert!(is_no_active_window(&error_value(
            "device_refused_action",
            "device reported: No active window available",
            false,
        )));
        assert!(!is_no_active_window(&error_value(
            "device_refused_action",
            "device reported: unknown tool",
            false,
        )));
        assert!(!is_no_active_window(&json!({
            "status": "ok",
            "result": { "content": [] },
        })));
    }
}
