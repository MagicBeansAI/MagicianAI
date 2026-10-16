//! Content-free feedback for a governed call which produced no admitted result.

use serde_json::{json, Value};

fn failed_call() -> Value {
    json!({
        "success": false,
        "outcome": "failed",
        "error_code": "app_tool_call_failed",
        "guidance": "The tool call failed; no result data is disclosed. Check the supplied tool contract and correct the arguments before retrying. For app_store_query, select only declared entity fields; record_id and record_revision are returned row metadata, not selectable fields. Sort directions are ascending or descending. Predicates use the declared root/nodes arena. For app_commit_mutations, operations use kind; creates need temporary_id and payload, updates need record_id and patch. expected_record_revisions entries use entity, record_id and revision (the row's record_revision). A failed call does not prove that the source has no data."
    })
}

/// Failure prose can contain private data. Replace it wholesale with host
/// constants, never interpret it as content authority or a successful result.
/// A checkpoint-bearing message still owes exact result/sequence validation.
pub(super) fn normalize_unlabeled_feedback(content: &mut Value) {
    if content.get("success").and_then(Value::as_bool) == Some(false)
        && content.get("app_result_checkpoint").is_none()
    {
        *content = failed_call();
    }
}

/// These exact host constants carry neither store data nor a checkpoint. A
/// deferred call was not executed and cannot consume a result sequence slot.
pub(super) fn is_content_free_feedback(content: &Value) -> bool {
    *content == failed_call() || *content == json!({"status":"deferred"})
}
