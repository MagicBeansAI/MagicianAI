//! The Android companion as a verification-code source (plan §6.2): a
//! trusted handoff, not a tool result.
//!
//! The resolver asks the connected, permitted companion — through the
//! device hub, the transport authority behind the four-verb roster — to
//! watch its notifications for *this* challenge. The companion extracts the
//! code on the device and answers the ask itself over its paired credential
//! (`POST /hitl/{id}/respond`, channel `android_notification`); what comes
//! back here is status. A companion whose `android_await_otp` does not
//! require a `challenge` is an older build that would return digits: it is
//! never called. Each call is one short slice of the window, so the owner's
//! grant is re-read between slices and withdrawing it stops the next one;
//! the answer path itself refuses a device the grant no longer covers.
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::matching::{ChallengeContext, SourceKind};
use super::sources::{AuthorizedSource, SourceSignal, SourceWatch, WatchPoll};
use crate::magician_v2::device_bridge::{global_hub, DeviceKey};

/// The device action the companion exposes on its own roster.
pub const DEVICE_ACTION: &str = "android_await_otp";
/// The argument an eligible companion requires; a roster without it is an
/// older build.
pub const CHALLENGE_ARGUMENT: &str = "challenge";
/// The `source` the companion answers under when the challenge names no lane
/// (an older announcement): the one installed sink answers agentic pauses, and
/// the respond endpoint routes by this field.
pub const ANSWER_SOURCE: &str = "agentic";
/// One hub call waits at most this long; the resolver polls again after.
pub const DEVICE_SLICE: Duration = Duration::from_secs(30);
/// How long to wait for the companion's session to be ready.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct AndroidVerificationWatch;

#[async_trait]
impl SourceWatch for AndroidVerificationWatch {
    fn kind(&self) -> SourceKind {
        SourceKind::AndroidNotification
    }

    async fn poll(
        &self,
        source: &AuthorizedSource,
        challenge: &ChallengeContext,
        _since_ms: i64,
        _limit: usize,
    ) -> Result<WatchPoll, String> {
        let Some(hub) = global_hub() else {
            return Err("no device hub".to_string());
        };
        let key = DeviceKey {
            principal: challenge.principal.clone(),
            workspace: challenge.workspace.clone(),
            device_id: source.account.clone(),
        };
        let eligible = hub
            .tool_requires(&key, DEVICE_ACTION, CHALLENGE_ARGUMENT, READY_TIMEOUT)
            .await
            .map_err(|error| format!("device roster unavailable: {error}"))?;
        if !eligible {
            return Ok(WatchPoll {
                messages: Vec::new(),
                exhausted: true,
                signal: SourceSignal::Unavailable(
                    "the companion app is too old to watch for a challenge; update it".to_string(),
                ),
            });
        }
        let now = chrono::Utc::now().timestamp_millis();
        let remaining_ms = challenge
            .deadline_ms
            .map(|deadline| (deadline - now).max(1_000))
            .unwrap_or(DEVICE_SLICE.as_millis() as i64)
            .min(DEVICE_SLICE.as_millis() as i64);
        let timeout_seconds = (remaining_ms / 1000).max(1);
        let params = device_call_params(challenge, timeout_seconds);
        let result = hub
            .dispatch(
                &key,
                DEVICE_ACTION,
                params,
                Duration::from_secs(timeout_seconds as u64 + 5),
            )
            .await
            .map_err(|error| format!("device call failed: {error}"))?;
        Ok(interpret_device_result(&result))
    }
}

/// What the companion is told about the challenge it is to watch for. Value
/// free, and it names the lane that owns the ask: the companion answers over
/// its own credential, so without `source` it guessed — and a wrong guess is
/// an answer the runtime refuses.
pub fn device_call_params(challenge: &ChallengeContext, timeout_seconds: i64) -> Value {
    json!({
        "timeout_seconds": timeout_seconds,
        "challenge": {
            "correlation_id": challenge.correlation_id,
            "source": if challenge.lane.is_empty() { ANSWER_SOURCE } else { challenge.lane.as_str() },
            "started_at_ms": challenge.started_at_ms,
            "window_start_ms": challenge.window_start_ms(),
            "deadline_ms": challenge.deadline_ms,
            "expected_digits": challenge.expected.digits,
            "expected_host": challenge.expected_host,
        },
    })
}

/// Read the companion's status into the source contract. A result carrying
/// digits is an old companion that slipped past the roster check: nothing
/// here reads them, and the source counts as unavailable.
pub fn interpret_device_result(result: &Value) -> WatchPoll {
    let payload = device_payload(result);
    if payload.get("code").is_some() {
        tracing::warn!("[VERIFICATION-CODES] the companion returned a raw code; an older bridge is not eligible");
        return WatchPoll {
            messages: Vec::new(),
            exhausted: true,
            signal: SourceSignal::Unavailable(
                "the companion app is too old to watch for a challenge; update it".to_string(),
            ),
        };
    }
    let status = payload.get("status").and_then(Value::as_str).unwrap_or("");
    let reason = payload
        .get("reason")
        .or_else(|| payload.get("message"))
        .and_then(Value::as_str)
        .map(safe_reason);
    let (exhausted, signal) = match status {
        "deposited" => (true, SourceSignal::AnsweredBySource),
        "already_resolved" => (true, SourceSignal::AlreadyResolved),
        "ambiguous" => (
            true,
            SourceSignal::Ambiguous(
                "the phone saw more than one code inside the window".to_string(),
            ),
        ),
        // The slice ended with nothing: the resolver polls again while the
        // challenge is open.
        "no_code" => (false, SourceSignal::None),
        "unavailable" => (
            true,
            SourceSignal::Unavailable(
                "notification access is not granted on the phone".to_string(),
            ),
        ),
        "deposit_failed" => (
            true,
            SourceSignal::Unavailable(format!(
                "the phone could not deliver the code: {}",
                reason.as_deref().unwrap_or("the answer was refused")
            )),
        ),
        "requires_challenge" => (
            true,
            SourceSignal::Unavailable(
                "the companion app did not accept the challenge; update it".to_string(),
            ),
        ),
        other => (
            true,
            SourceSignal::Unavailable(format!("the phone reported `{}`", safe_status(other))),
        ),
    };
    WatchPoll {
        messages: Vec::new(),
        exhausted,
        signal,
    }
}

/// A reason sentence the phone sent, kept to printable text of a bounded
/// length: it rides into a status event and the prompt's line.
fn safe_reason(reason: &str) -> String {
    reason
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A status word the phone sent, kept to a short identifier for the log.
fn safe_status(status: &str) -> String {
    status
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect()
}

/// The companion's JSON, whether it came back as a parsed object or as a
/// text content block.
fn device_payload(result: &Value) -> Value {
    if result.get("status").is_some() || result.get("code").is_some() {
        return result.clone();
    }
    let text = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find_map(|item| item.get("text").and_then(Value::as_str))
        })
        .or_else(|| result.as_str());
    text.and_then(|text| serde_json::from_str::<Value>(text).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The companion answers `/hitl/{id}/respond` itself, and that endpoint
    /// routes by `source`. Leaving it out made the phone guess `user_request`,
    /// a lane with no record of an agentic pause: every deposit was refused.
    #[test]
    fn the_call_names_the_lane_that_owns_the_ask_and_the_window_it_may_read() {
        use super::super::extract::ExpectedFormat;
        use super::super::matching::ChallengeContext;
        let challenge = ChallengeContext {
            principal: "owner".into(),
            workspace: "ws".into(),
            correlation_id: "exec-1:plan:step~0123456789ab".into(),
            started_at_ms: 1_000_000,
            deadline_ms: Some(1_300_000),
            expected_host: Some("accounts.example.test".into()),
            expected: ExpectedFormat { digits: Some(6) },
            lookback_ms: 120_000,
            not_before_ms: Some(990_000),
            lane: "agentic".to_string(),
        };
        let params = device_call_params(&challenge, 30);
        let sent = &params["challenge"];
        assert_eq!(sent["source"], "agentic", "the lane the announcement named");
        assert_eq!(sent["correlation_id"], "exec-1:plan:step~0123456789ab");
        assert_eq!(
            sent["window_start_ms"], 990_000,
            "the clamp travels to the phone too"
        );
        assert_eq!(params["timeout_seconds"], 30);
        // Nothing about a code, ever.
        assert!(!params.to_string().contains("code\""), "{params}");
    }

    #[test]
    fn a_deposit_is_the_only_success_and_every_other_status_is_told_honestly() {
        let deposited = interpret_device_result(&json!({"status": "deposited", "waited_ms": 1200}));
        assert_eq!(deposited.signal, SourceSignal::AnsweredBySource);
        assert!(deposited.exhausted);
        let wrapped = interpret_device_result(
            &json!({"content": [{"type": "text", "text": "{\"status\":\"deposited\"}"}]}),
        );
        assert_eq!(wrapped.signal, SourceSignal::AnsweredBySource);
        let nothing = interpret_device_result(&json!({"status": "no_code"}));
        assert_eq!(nothing.signal, SourceSignal::None);
        assert!(
            !nothing.exhausted,
            "a slice that ends empty is polled again"
        );
        assert!(matches!(
            interpret_device_result(&json!({"status": "ambiguous", "candidates": 2})).signal,
            SourceSignal::Ambiguous(_)
        ));
        assert_eq!(
            interpret_device_result(&json!({"status": "already_resolved"})).signal,
            SourceSignal::AlreadyResolved
        );
        assert!(
            matches!(interpret_device_result(&json!({"status": "unavailable"})).signal, SourceSignal::Unavailable(r) if r.contains("access"))
        );
        assert!(matches!(
            interpret_device_result(&json!({"status": "deposit_failed", "reason": "Magician answered HTTP 403"})).signal,
            SourceSignal::Unavailable(r) if r.contains("403")
        ));
        let long = format!("x{}\n<b>", "y".repeat(500));
        assert!(matches!(
            interpret_device_result(&json!({"status": "deposit_failed", "reason": long})).signal,
            SourceSignal::Unavailable(r) if r.len() < 160 && !r.contains('\n')
        ));
        assert!(matches!(
            interpret_device_result(&json!({"status": "requires_challenge"})).signal,
            SourceSignal::Unavailable(_)
        ));
        assert!(
            matches!(interpret_device_result(&json!({"status": "weird<script>"})).signal, SourceSignal::Unavailable(r) if r.contains("weirdscript"))
        );
        // Digits disqualify the bridge, and are never read.
        let old = interpret_device_result(&json!({"code": "123456", "sender": "VERIFY"}));
        assert!(matches!(&old.signal, SourceSignal::Unavailable(r) if r.contains("too old")));
        assert!(old.exhausted && old.messages.is_empty());
        assert!(!format!("{:?}", old.signal).contains("123456"));
        let old_wrapped = interpret_device_result(
            &json!({"content": [{"type": "text", "text": "{\"code\":\"123456\"}"}]}),
        );
        assert!(matches!(old_wrapped.signal, SourceSignal::Unavailable(_)));
    }
}
