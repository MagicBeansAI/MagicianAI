//! `web_fetch` compatibility facade over the scoped content-read controller.
//!
//! Production reads use the same SSRF-safe fetch, cache, local extraction,
//! quality, deadline, and policy path as `content_read`.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

pub async fn handle(resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    if resources.content_acquisition_resolver().is_some() {
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut retrieval =
            super::content_read::execute_and_materialize(Arc::clone(&resources), args).await?;
        if let Some(retrieval_object) = retrieval.as_object_mut() {
            retrieval_object.insert("compatibility_prompt".into(), Value::String(prompt.clone()));
        }
        return Ok(project_retrieval_result(retrieval, &prompt));
    }
    Err(ExecutionError::Configuration(
        "web_fetch requires the scoped content acquisition resolver".into(),
    ))
}

/// Project the typed retrieval into the compatibility shape.
///
/// The retrieval already ran the shared per-document eligibility decision
/// (`content_read::project_scalar_evidence_classification`), and its verdict —
/// `fetch_status`, `evidence_role`, `claim_eligible` — is carried to the root
/// here, beside `canonical_url`, exactly as the governed reader reports it.
/// The terminal grounding judge admits evidence by that envelope's shape, so
/// a page read through this facade is verifiable on the same terms as one
/// read through `content_read`; without the hoist every fact reached through
/// the persona's sanctioned compatibility path was structurally unverifiable.
/// `excerpt` is the bounded copy of the text that survives model projection
/// when the full `content` is over the scalar budget.
fn project_retrieval_result(retrieval: Value, prompt: &str) -> Value {
    if let Some(document) = retrieval.get("document").filter(|value| !value.is_null()) {
        let text = document.get("text").and_then(Value::as_str).unwrap_or("");
        let (excerpt, excerpt_complete) = super::content_read::opened_page_excerpt(text);
        let mut projected = json!({
            "status": "ok",
            "url": document.get("canonical_url"),
            "canonical_url": document.get("canonical_url"),
            "content_type": document.get("media_type"),
            "title": document.get("title"),
            "content_chars": document.get("text").and_then(Value::as_str).map(|text| text.chars().count()),
            "prompt_hint": prompt,
            "content": document.get("text"),
            "excerpt": excerpt,
            "excerpt_complete": excerpt_complete,
            "provenance": document.get("provenance"),
            "retrieval": retrieval,
        });
        for key in ["fetch_status", "evidence_role", "claim_eligible"] {
            if let Some(verdict) = projected["retrieval"].get(key).cloned() {
                projected[key] = verdict;
            }
        }
        return projected;
    }
    json!({
        "status": "error",
        "reason": "configured static-reading ladder did not return usable content",
        "retrieval": retrieval,
    })
}

/// Strip HTML to readable text. Removes `<script>` / `<style>` blocks,
/// drops remaining tags, decodes the common entities, and collapses
/// whitespace. Lossy but good enough to feed an LLM a clean reading
/// surface — also reused crate-wide to extract a readable summary from an
/// HTML task deliverable (the chat task-result card + the `/tasks` listing).
pub fn strip_html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    while i < bytes.len() {
        if let Some(closer) = skip_until {
            if let Some(end) = lower[i..].find(closer) {
                i += end + closer.len();
                skip_until = None;
                continue;
            } else {
                break;
            }
        }
        let ch = html[i..]
            .chars()
            .next()
            .expect("byte offset remains on a UTF-8 boundary");
        if !in_tag && ch == '<' {
            let tail = &lower[i..];
            if tail.starts_with("<script") {
                skip_until = Some("</script>");
                i += 7;
                continue;
            }
            if tail.starts_with("<style") {
                skip_until = Some("</style>");
                i += 6;
                continue;
            }
            in_tag = true;
            i += 1;
            continue;
        }
        if in_tag {
            if ch == '>' {
                in_tag = false;
                out.push(' ');
            }
            i += ch.len_utf8();
            continue;
        }
        out.push(ch);
        i += ch.len_utf8();
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    let mut collapsed = String::with_capacity(decoded.len());
    let mut prev_ws = false;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }
    collapsed.trim().to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn html_text_preserves_unicode_outside_tags() {
        assert_eq!(strip_html_to_text("<p>Résumé · 研究</p>"), "Résumé · 研究");
    }

    #[test]
    fn compatibility_projection_preserves_the_exact_typed_retrieval_envelope() {
        let retrieval = json!({
            "trace_id": "trace-1",
            "status": "complete",
            "document": {
                "title": "Public article",
                "text": "Bounded public evidence",
                "canonical_url": "https://example.com/article",
                "media_type": "text/html",
                "provenance": {
                    "source_label": "Example",
                    "source_url": "https://example.com/article",
                    "retrieved_by": "static-http"
                }
            },
            "attempts": [{"action_id": "static_http.read"}]
            ,"working_set": {
                "status": "created",
                "working_set_id": "ws-evidence",
                "source_count": 1,
                "chunk_count": 1
            }
        });

        let projected = project_retrieval_result(retrieval.clone(), "focus on evidence");

        assert_eq!(projected["status"], json!("ok"));
        assert_eq!(projected["content_chars"], json!(23));
        assert_eq!(projected["content"], retrieval["document"]["text"]);
        assert_eq!(projected["excerpt"], retrieval["document"]["text"]);
        assert_eq!(projected["excerpt_complete"], json!(true));
        assert_eq!(
            projected["canonical_url"],
            retrieval["document"]["canonical_url"]
        );
        assert_eq!(projected["provenance"], retrieval["document"]["provenance"]);
        assert_eq!(projected["retrieval"], retrieval);
        assert_eq!(
            projected["retrieval"]["working_set"]["working_set_id"],
            "ws-evidence"
        );
        // No envelope on the retrieval, none invented at the root.
        assert!(projected.get("claim_eligible").is_none());
    }

    #[test]
    fn compatibility_projection_carries_the_retrievals_eligibility_verdict_to_the_root() {
        let retrieval = json!({
            "trace_id": "trace-3",
            "status": "complete",
            "fetch_status": "complete",
            "evidence_role": "opened_page",
            "claim_eligible": true,
            "document": {
                "title": "Release notes",
                "text": "Version 1.98.1 was released.",
                "canonical_url": "https://example.com/releases",
                "media_type": "text/html",
                "provenance": {
                    "source_label": "Example",
                    "source_url": "https://example.com/releases",
                    "retrieved_by": "static-http"
                }
            }
        });

        let projected = project_retrieval_result(retrieval.clone(), "");

        assert_eq!(projected["fetch_status"], json!("complete"));
        assert_eq!(projected["evidence_role"], json!("opened_page"));
        assert_eq!(projected["claim_eligible"], json!(true));
        assert_eq!(
            projected["canonical_url"],
            json!("https://example.com/releases")
        );

        let mut degraded = retrieval.clone();
        degraded["claim_eligible"] = json!(false);
        degraded["evidence_role"] = json!("discovery_only");
        let projected = project_retrieval_result(degraded, "");
        assert_eq!(projected["claim_eligible"], json!(false));
        assert_eq!(projected["evidence_role"], json!("discovery_only"));
    }

    #[test]
    fn compatibility_projection_keeps_typed_degraded_evidence_without_fabricating_content() {
        let retrieval = json!({
            "trace_id": "trace-2",
            "status": "degraded",
            "document": null,
            "attempts": [{
                "action_id": "static_http.read",
                "classification": "javascript_required"
            }]
        });

        let projected = project_retrieval_result(retrieval.clone(), "");

        assert_eq!(projected["status"], json!("error"));
        assert_eq!(projected["retrieval"], retrieval);
        assert!(projected.get("content").is_none());
    }
}
