//! Best-effort Ollama model pre-warm for local pre-summarization.
//!
//! When `llm.dispatch.local_prep.enabled` is set, the first local-prep call
//! would otherwise pay a cold model-load (into VRAM) latency. This spawns a
//! tiny `generate` request at boot to warm the model. No-op when local-prep is
//! disabled; failures (e.g. Ollama not running) are logged and ignored.
//!
//! ALLOWLISTED DIRECT-OLLAMA EXCEPTION. Per the "ollama single chokepoint"
//! principle every ollama *inference* call goes through `magicllm`'s
//! `OllamaProvider`; this file is the one sanctioned bypass because it is NOT
//! inference — it is a `num_predict:1` model-load ping whose output is discarded.
//! Routing a load-ping through the provider (or the dispatch queue) would add no
//! value and, for the queue, risk worker-pool recursion. It still sends
//! `think:false` so reasoning models don't spend the warm-up on thinking tokens.
//! This exception is encoded in `scripts/check_ollama_single_chokepoint.py`; any
//! OTHER `/api/generate` or `/api/chat` POST outside the allowlist fails that gate.

use std::time::Duration;

use magicllm::LocalPrepConfig;
use tracing::{info, warn};

/// Spawn a best-effort prewarm of the local-prep Ollama model. Returns
/// immediately (work happens on a detached task). No-op unless
/// `config.enabled`. This is the ONE allowlisted direct-ollama exception (a
/// model-load ping, not inference — see the module doc); it does NOT go through
/// `OllamaProvider` or the dispatch queue.
pub fn spawn_local_prep_prewarm(config: LocalPrepConfig) {
    if !config.enabled {
        return;
    }
    tokio::spawn(async move {
        let url = format!("{}/api/generate", config.base_url.trim_end_matches('/'));
        // think:false — a model-load ping, not a reasoning task; keep reasoning
        // models from spending the warm-up on thinking tokens. This direct call
        // bypasses OllamaProvider, so it sets the flag itself.
        let mut body = serde_json::json!({
            "model": config.model,
            "prompt": "ok",
            "stream": false,
            "think": false,
            "options": {
                "num_ctx": config.context_tokens,
                "num_predict": 1,
            },
        });
        if let Some(keep_alive) = config
            .keep_alive
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body["keep_alive"] = serde_json::json!(keep_alive);
        }
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs.max(10)))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                warn!(error = %error, "local-prep prewarm: HTTP client build failed");
                return;
            },
        };
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!(model = %config.model, "local-prep Ollama model pre-warmed");
            },
            Ok(resp) => {
                warn!(status = %resp.status(), model = %config.model, "local-prep prewarm: non-success status");
            },
            Err(error) => {
                warn!(error = %error, model = %config.model, "local-prep prewarm failed (Ollama unreachable?)");
            },
        }
    });
}
