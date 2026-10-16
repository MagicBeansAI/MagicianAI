//! Recover captured desktop images from recorded results or the live transcript.
//! This is transport only; all next-action policy belongs to Decision Engine.
use super::types::ExecutionHistory;
use crate::magician_v2::execution::actions::ExecutableAction;

fn is_snapshot(action: &ExecutableAction) -> bool {
    matches!(action, ExecutableAction::Pack { capability_name, resolved_params, .. }
        if capability_name == super::decision::DESKTOP_CALL_CAPABILITY
        && resolved_params.get("action_name").and_then(serde_json::Value::as_str)
            == Some("get_window_state"))
}

pub(crate) fn snapshot_replies_from_transcript(
    history: &ExecutionHistory,
) -> Vec<serde_json::Value> {
    history
        .live_messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            magicllm::prelude::ContentBlock::ToolResult { content, .. } => {
                desktop_reply(content).cloned()
            },
            magicllm::prelude::ContentBlock::Text { text } => {
                serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|value| desktop_reply(&value).cloned())
            },
            _ => None,
        })
        .collect()
}

/// The reply of the snapshot record at `index`: its own output when that
/// reads, else the transcript's, matched by position — the k-th snapshot
/// record from the end is the k-th reply from the end — and in either case
/// returned for image transport. Accessibility-tree hydration is unnecessary here.
pub(crate) fn snapshot_reply_at(
    history: &ExecutionHistory,
    index: usize,
) -> Option<serde_json::Value> {
    let record = history.iterations.get(index)?;
    let recorded = record
        .result
        .output
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| desktop_reply(&value).cloned());
    let reply = match recorded {
        Some(reply) => reply,
        None => {
            let from_end = history.iterations[index + 1..]
                .iter()
                .filter(|later| is_snapshot(&later.action) && later.result.success)
                .count();
            let replies = snapshot_replies_from_transcript(history);
            replies
                .len()
                .checked_sub(from_end + 1)
                .and_then(|position| replies.get(position).cloned())?
        },
    };
    Some(reply)
}

fn desktop_reply(value: &serde_json::Value) -> Option<&serde_json::Value> {
    let is_reply = |candidate: &serde_json::Value| {
        candidate.is_object()
            && (candidate.get("elements").is_some()
                || candidate.get("tree_markdown").is_some()
                || candidate.get("screenshot_file").is_some())
    };
    [
        Some(value),
        value.get("result"),
        value.get("result").and_then(|r| r.get("data")),
        value.get("data"),
        value
            .get("content")
            .and_then(|c| c.get("result"))
            .and_then(|r| r.get("data")),
    ]
    .into_iter()
    .flatten()
    .find(|candidate| is_reply(candidate))
}
