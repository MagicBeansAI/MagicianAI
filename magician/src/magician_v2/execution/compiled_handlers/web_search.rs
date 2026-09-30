//! `web_search` — query DuckDuckGo's HTML endpoint and parse the top
//! results into a structured list.
//!
//! Pure-Rust scraping (mirrors the legacy `websearch` skill's approach
//! but doesn't require Python on the host). Applies optional
//! `allowed_domains` / `blocked_domains` filters on the URL host.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::magician_v2::execution::agent_resources::AgentResources;
use crate::magician_v2::execution::error::ExecutionError;

const MAX_RESULTS: usize = 10;
const FETCH_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone)]
pub struct PublicWebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

pub async fn handle(_resources: Arc<AgentResources>, args: Value) -> Result<Value, ExecutionError> {
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return Ok(json!({
            "status": "error",
            "reason": "web_search requires `query` (string).",
        }));
    };
    let allowed_domains: Vec<String> = args
        .get("allowed_domains")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let blocked_domains: Vec<String> = args
        .get("blocked_domains")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|d| d.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let results =
        match search_public_web(query, &allowed_domains, &blocked_domains, MAX_RESULTS).await {
            Ok(results) => results,
            Err(error) => {
                return Ok(json!({
                    "status": "error",
                    "reason": error.to_string(),
                }));
            },
        };

    Ok(json!({
        "status": "ok",
        "query": query,
        "result_count": results.len(),
        "results": results.into_iter().map(|result| json!({
            "title": result.title,
            "url": result.url,
            "snippet": result.snippet,
        })).collect::<Vec<_>>(),
        "allowed_domains": allowed_domains,
        "blocked_domains": blocked_domains,
        "source": "duckduckgo_html",
    }))
}

pub async fn search_public_web(
    query: &str,
    allowed_domains: &[String],
    blocked_domains: &[String],
    limit: usize,
) -> Result<Vec<PublicWebSearchResult>> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0 Safari/537.36",
        )
        .build()
        .context("building DuckDuckGo HTTP client")?;
    let response = client
        .get(&url)
        .send()
        .await
        .context("requesting DuckDuckGo search")?
        .error_for_status()
        .context("DuckDuckGo search returned an error status")?;
    let html = response
        .text()
        .await
        .context("reading DuckDuckGo response")?;

    let result_re = regex::Regex::new(
        r#"(?s)class="result__a"[^>]*href="//duckduckgo\.com/l/\?uddg=([^&]*)&[^>]*>([^<]*)</a>.*?class="result__snippet"[^>]*>(.*?)</[at]"#,
    )
    .context("compiling DuckDuckGo result parser")?;
    let tag_re = regex::Regex::new(r"<[^>]+>").context("compiling DuckDuckGo snippet parser")?;

    let host_of = |raw_url: &str| -> Option<String> {
        let stripped = raw_url
            .strip_prefix("https://")
            .or_else(|| raw_url.strip_prefix("http://"))
            .unwrap_or(raw_url);
        stripped
            .split('/')
            .next()
            .map(|host| host.trim_end_matches(':').to_lowercase())
    };

    let mut results = Vec::new();
    for cap in result_re.captures_iter(&html) {
        if results.len() >= limit.min(MAX_RESULTS) {
            break;
        }
        let raw_url = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let decoded_url = urlencoding::decode(raw_url)
            .map(|s| s.into_owned())
            .unwrap_or_else(|_| raw_url.to_string());
        let title = cap
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let raw_snippet = cap.get(3).map(|m| m.as_str()).unwrap_or("");
        let snippet = tag_re.replace_all(raw_snippet, "").trim().to_string();

        if !allowed_domains.is_empty() {
            let host = host_of(&decoded_url).unwrap_or_default();
            if !allowed_domains.iter().any(|d| {
                host == d.to_lowercase() || host.ends_with(&format!(".{}", d.to_lowercase()))
            }) {
                continue;
            }
        }
        if !blocked_domains.is_empty() {
            let host = host_of(&decoded_url).unwrap_or_default();
            if blocked_domains.iter().any(|d| {
                host == d.to_lowercase() || host.ends_with(&format!(".{}", d.to_lowercase()))
            }) {
                continue;
            }
        }

        results.push(PublicWebSearchResult {
            title,
            url: decoded_url,
            snippet,
        });
    }
    Ok(results)
}
