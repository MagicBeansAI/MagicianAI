//! `web_answer` — one-shot provider-executed web search.
//!
//! The model calls this tool with a query; execution makes ONE LLM request
//! through the operation router whose profile carries the
//! `server_web_search` flag, so the provider (OpenAI Responses, Anthropic,
//! Gemini, or OpenRouter — whichever transport the `web_answer` operation
//! maps to) searches server-side and returns a grounded answer with
//! citations. The request deliberately carries NO function tools: the
//! no-mixing invariant between server-side search and tool dispatch is
//! enforced at the transport layer and respected here by construction.
//!
//! Deep, multi-source research stays with the research agent; this tool is
//! the cheap single-lookup lane. Enable/disable is config-borne: the tool
//! works exactly when the `web_answer` operation has a profile mapping.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

/// Kept short so the provider's search-grounded answer leads with the
/// fact and cites inline, rather than padding.
const WEBSEARCH_SYSTEM_PROMPT: &str = "You have live web search. Answer the \
user's query concisely and lead with the fact. Prefer primary sources. If \
the search results do not answer the query, say so plainly instead of \
guessing.";

/// Extract the required, non-empty `query` argument. Returns the error
/// payload (not an `Err`) so the model sees a structured reason.
fn require_query(args: &Value) -> Result<&str, Value> {
    args.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            json!({
                "status": "error",
                "reason": "web_answer requires a non-empty `query`",
            })
        })
}

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let query = match require_query(&args) {
        Ok(query) => query,
        Err(error) => return Ok(error),
    };

    // Same governance discipline as `analyze_image_via_openai`: a provider
    // call without tenant/call identity is refused, never made unobserved.
    let call_context =
        crate::magician_v2::execution::compiled_dispatch::current_compiled_llm_call_context()
            .ok_or_else(|| {
                ExecutionError::Configuration(
                    "web_answer: compiled dispatch did not supply valid tenant and call identity"
                        .to_string(),
                )
            })?;
    let Some(router) =
        crate::magician_v2::query_analysis::operation_llm_router::global_operation_router()
    else {
        return Err(ExecutionError::Configuration(
            "web_answer: operation router is not initialised".to_string(),
        ));
    };

    let operation = crate::magician_v2::query_analysis::operation_llm_router::LLMOperation::Other(
        "web_answer".to_string(),
    );
    // Enforce the enable/disable contract: an unmapped operation must NOT
    // silently fall back to the router's default profile and answer without
    // search — the model would receive `status: ok` for an un-grounded
    // answer it was told is web-grounded, on flagship-profile spend.
    let profile = router
        .get_config_for_operation(&operation)
        .map_err(|error| {
            ExecutionError::Configuration(format!(
                "web_answer: no profile routed for the operation: {error:#}"
            ))
        })?;
    let search_enabled = profile
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("server_web_search"))
        .is_some_and(|flag| flag.as_bool().unwrap_or(true));
    if !search_enabled {
        return Ok(json!({
            "status": "error",
            "reason": "web_answer is not enabled: the `web_answer` operation has no \
                       mapping to a profile with `server_web_search` metadata",
        }));
    }
    // Empty function tools: the server-side search tool comes from the
    // profile's `server_web_search` flag, and the two must never mix.
    let response = router
        .generate_for_execution_native_tools_with_trace(
            &operation,
            Some(WEBSEARCH_SYSTEM_PROMPT),
            query,
            Vec::new(),
            None,
            None,
            None,
            None,
            Some(call_context.trace_context.clone()),
        )
        .await
        .map_err(|error| {
            ExecutionError::Step(format!("web_answer model request failed: {error:#}"))
        })?;

    let Some(answer) = response
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    else {
        return Ok(json!({
            "status": "error",
            "reason": "web_answer model returned an empty answer",
        }));
    };

    let (searches, citations) = response
        .web_search
        .map(|summary| (summary.searches, summary.citations))
        .unwrap_or((0, Vec::new()));

    Ok(json!({
        "status": "ok",
        "query": query,
        "answer": answer,
        "searches": searches,
        "citations": citations
            .iter()
            .map(|citation| json!({
                "url": citation.url,
                "title": citation.title,
            }))
            .collect::<Vec<_>>(),
        "model": response.model,
        "provider": response.provider,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_query_rejects_missing_blank_and_non_string() {
        for args in [
            json!({}),
            json!({"query": "   "}),
            json!({"query": 42}),
            json!({"query": ""}),
        ] {
            let error = require_query(&args).expect_err("must reject");
            assert_eq!(error["status"], "error");
            assert!(error["reason"].as_str().unwrap().contains("query"));
        }
    }

    #[test]
    fn require_query_trims_and_accepts_plain_questions() {
        let args = json!({"query": "  latest rust version?  "});
        assert_eq!(require_query(&args).unwrap(), "latest rust version?");
    }
}
