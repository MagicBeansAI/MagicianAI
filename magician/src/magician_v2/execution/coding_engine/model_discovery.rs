//! Runtime model discovery for the harness CLIs and provider APIs.
//!
//! The background worker records what each source offers into
//! [`magicllm::model_discovery`], which the coding and routing model APIs read
//! (availability, uncatalogued models, drift). Nothing here runs on a request
//! path: request handlers only read the cached result.
//!
//! Sources:
//! - Antigravity: `agy models` (`id\tLabel` rows), Magician's filtered env.
//! - Grok: `grok models` in Magician's own Grok home (the CLI's selectable
//!   list; its `models_cache.json` also lists image/video models).
//! - Codex: the CLI's own `models_cache.json` under `CODEX_HOME` (rows with
//!   `visibility: hide` are not offered).
//! - Claude Code: no listing; a run that names a rejected model records it.
//! - Provider APIs (OpenAI, Anthropic, Gemini, xAI, DeepSeek, OpenRouter,
//!   Ollama): their model-list endpoints, where a key is configured.
//!
//! A harness or provider error that refuses a model by name is recorded too
//! ([`observe_engine_failure`], the plane one-shot session, and the magicllm
//! harness provider), and asks for an early refresh of that source.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};

use magicllm::{
    model_discovery::{self as discovery, ListingOrigin, SourceDrift},
    LLMRouterConfig,
};
use serde_json::{json, Value};

use super::{CodingEngineKind, CodingEngineRequest};
use crate::config::{MagicianAgySettings, MagicianCodexSettings, MagicianGrokSettings};

pub const SOURCE_CLAUDE: &str = "harness-claude_code";
pub const SOURCE_CODEX: &str = "harness-codex";
pub const SOURCE_GROK: &str = "harness-grok";
pub const SOURCE_AGY: &str = "harness-agy";

/// Native coding engines: (engine id, default profile id, discovery source).
pub const NATIVE_ENGINE_SOURCES: [(&str, &str, &str); 4] = [
    ("claude_code", "claude-default", SOURCE_CLAUDE),
    ("codex_app_server", "codex-default", SOURCE_CODEX),
    ("grok_acp", "grok-default", SOURCE_GROK),
    ("agy_cli", "agy-default", SOURCE_AGY),
];

/// How long a CLI listing is trusted before the worker asks again.
pub const CLI_TTL: Duration = Duration::from_secs(15 * 60);
/// How long a provider listing is trusted before the worker asks again.
pub const PROVIDER_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const WORKER_INTERVAL: Duration = Duration::from_secs(60);
const STARTUP_DELAY: Duration = Duration::from_secs(20);
const CONFIG_RELOAD: Duration = Duration::from_secs(10 * 60);
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CACHE_BYTES: u64 = 4 * 1024 * 1024;

/// The discovery source of a native coding engine. Pi runs API models, whose
/// sources are their providers.
pub fn engine_source(engine: CodingEngineKind) -> Option<&'static str> {
    match engine {
        CodingEngineKind::ClaudeCode => Some(SOURCE_CLAUDE),
        CodingEngineKind::CodexAppServer => Some(SOURCE_CODEX),
        CodingEngineKind::GrokAcp => Some(SOURCE_GROK),
        CodingEngineKind::AgyCli => Some(SOURCE_AGY),
        CodingEngineKind::Pi => None,
    }
}

/// The model a turn asked its engine for, when it pinned one.
pub fn requested_model(engine: CodingEngineKind, request: &CodingEngineRequest) -> Option<&str> {
    match engine {
        CodingEngineKind::ClaudeCode => request.claude.model.as_deref(),
        CodingEngineKind::CodexAppServer => request.codex.model.as_deref(),
        CodingEngineKind::GrokAcp => request.grok.model.as_deref(),
        CodingEngineKind::AgyCli => request.agy.model.as_deref(),
        CodingEngineKind::Pi => None,
    }
}

/// Record a failed turn's error when it is the engine refusing its model.
pub fn observe_engine_failure(
    engine: CodingEngineKind,
    request: &CodingEngineRequest,
    error: &str,
) -> bool {
    match (engine_source(engine), requested_model(engine, request)) {
        (Some(source), Some(model)) => discovery::record_rejection(source, model, error),
        _ => false,
    }
}

/// The discovery source of a plane harness CLI, by its binary name.
pub fn source_for_cli_binary(binary: &Path) -> Option<&'static str> {
    match binary.file_stem()?.to_str()? {
        "claude" => Some(SOURCE_CLAUDE),
        "codex" => Some(SOURCE_CODEX),
        "grok" => Some(SOURCE_GROK),
        "agy" => Some(SOURCE_AGY),
        _ => None,
    }
}

/// Offered model slugs in a Codex `models_cache.json` document.
pub fn parse_codex_models_cache(body: &Value) -> Vec<String> {
    body.get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter(|model| model.get("visibility").and_then(Value::as_str) != Some("hide"))
                .filter_map(|model| model.get("slug").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Model ids in a CLI listing's output, or why there are none. A CLI that
/// is not signed in lists a fallback set, which is not what a run gets.
pub fn listing_from_cli_output(text: &str) -> Result<Vec<String>, String> {
    let lower = text.to_ascii_lowercase();
    if [
        "not authenticated",
        "not logged in",
        "not signed in",
        "login required",
        "use /login",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return Err("the CLI is not signed in".to_string());
    }
    let models = discovery::parse_model_listing(text);
    if models.is_empty() {
        Err("the CLI listed no models".to_string())
    } else {
        Ok(models)
    }
}

fn record(source: &str, result: Result<Vec<String>, String>, origin: ListingOrigin) {
    match result {
        Ok(models) => {
            discovery::record_listing(source, models, origin);
        },
        Err(error) => {
            tracing::debug!(target: "model_discovery", source, %error, "model listing failed; keeping the last good list");
            discovery::record_failure(source, &error);
        },
    }
}

async fn run_listing(
    binary: &Path,
    args: &[&str],
    env: impl IntoIterator<Item = (String, String)>,
) -> Result<String, String> {
    // `binary` is the resolved absolute path, so clearing the env cannot
    // change which program runs.
    let mut command = tokio::process::Command::new(binary);
    command
        .args(args)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in env {
        command.env(key, value);
    }
    let output = tokio::time::timeout(CLI_TIMEOUT, command.output())
        .await
        .map_err(|_| "the CLI listing timed out".to_string())?
        .map_err(|_| "the CLI listing could not start".to_string())?;
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(code) => format!("the CLI listing exited with status {code}"),
            None => "the CLI listing was stopped by a signal".to_string(),
        });
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    Ok(text)
}

fn inherited_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

async fn refresh_agy(settings: &MagicianAgySettings) {
    if !discovery::needs_refresh(SOURCE_AGY, CLI_TTL) {
        return;
    }
    let Some(binary) = super::discovery::resolved_agy_executable(
        settings,
        &super::discovery::AgySearchPaths::production(),
    ) else {
        return;
    };
    let use_api_key =
        settings.use_api_key || crate::magician_v2::harness_auth::uses_magician_credentials("agy");
    let inherited = inherited_env();
    let env = super::agy::filter_agy_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        use_api_key,
    );
    let result = run_listing(&binary, &["models"], env)
        .await
        .and_then(|text| listing_from_cli_output(&text));
    record(SOURCE_AGY, result, ListingOrigin::CliListing);
}

async fn refresh_grok(settings: &MagicianGrokSettings) {
    if !discovery::needs_refresh(SOURCE_GROK, CLI_TTL) {
        return;
    }
    let Some(binary) = super::discovery::resolved_grok_executable(
        settings,
        &super::discovery::GrokSearchPaths::production(),
    ) else {
        return;
    };
    let home = tokio::task::spawn_blocking(super::grok_contract::magician_grok_home)
        .await
        .ok()
        .flatten();
    let Some(home) = home else {
        discovery::record_failure(SOURCE_GROK, "Magician's Grok home could not be prepared");
        return;
    };
    let inherited = inherited_env();
    let mut env = super::grok::filter_grok_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    if !crate::magician_v2::harness_auth::uses_magician_credentials("grok") {
        env.remove("XAI_API_KEY");
    }
    // The same contained home a run uses: its sign-in, not the owner's
    // plugins or MCP servers.
    for (key, value) in super::grok_contract::grok_contained_env(&home) {
        env.insert(key, value);
    }
    let result = run_listing(&binary, &["models"], env)
        .await
        .and_then(|text| listing_from_cli_output(&text));
    record(SOURCE_GROK, result, ListingOrigin::CliListing);
}

fn codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

fn refresh_codex_from_cache(settings: &MagicianCodexSettings) {
    if !discovery::needs_refresh(SOURCE_CODEX, CLI_TTL) {
        return;
    }
    if super::discovery::resolved_codex_executable(
        settings,
        &super::discovery::CodexSearchPaths::production(),
    )
    .is_none()
    {
        return;
    }
    let Some(path) = codex_home().map(|home| home.join("models_cache.json")) else {
        return;
    };
    let result = match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MAX_CACHE_BYTES => {
            Err("Codex's model cache is too large".to_string())
        },
        Ok(_) => std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(|body| parse_codex_models_cache(&body))
            .filter(|models| !models.is_empty())
            .ok_or_else(|| "Codex's model cache was unreadable or empty".to_string()),
        Err(_) => Err("Codex has not cached its model list yet".to_string()),
    };
    record(SOURCE_CODEX, result, ListingOrigin::CliCache);
}

fn load_router() -> Option<LLMRouterConfig> {
    crate::config::load_magician_config_from_path(&crate::config::magician_config_path())
        .ok()
        .and_then(|config| config.router_config().cloned())
}

/// One drift notice per source, when its drift changes: a warning log and an
/// unscoped observability alert. Scoped Attention notices are raised by the
/// service-health lane from the published drift.
fn announce(drift: &SourceDrift) {
    tracing::warn!(
        target: "model_discovery",
        source = %drift.source,
        missing = ?drift.missing_models,
        uncatalogued = drift.uncatalogued.len(),
        "the model catalog has drifted from what the source offers"
    );
    if let Some(bus) = crate::magician_v2::decision_host::telemetry_broadcaster() {
        bus.observability_alert(
            &format!("model_catalog:{}", drift.source),
            "model_catalog_drift",
            json!(drift),
        );
    }
}

/// One worker pass: due CLI listings, due provider listings, then drift.
pub async fn tick_model_discovery(router: Option<&LLMRouterConfig>) {
    let coding = super::coding_budget_settings();
    refresh_agy(&coding.agy).await;
    refresh_grok(&coding.grok).await;
    let codex = coding.codex.clone();
    let _ = tokio::task::spawn_blocking(move || refresh_codex_from_cache(&codex)).await;
    let Some(router) = router else {
        return;
    };
    discovery::refresh_provider_listings(&router.catalog, PROVIDER_TTL).await;
    for drift in discovery::publish_drift(&router.catalog) {
        announce(&drift);
    }
}

/// Start the background discovery worker once per process.
pub fn spawn_model_discovery_worker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        tokio::time::sleep(STARTUP_DELAY).await;
        let mut interval = tokio::time::interval(WORKER_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut router: Option<(Instant, Option<LLMRouterConfig>)> = None;
        loop {
            interval.tick().await;
            if router
                .as_ref()
                .is_none_or(|(loaded, _)| loaded.elapsed() >= CONFIG_RELOAD)
            {
                let loaded = tokio::task::spawn_blocking(load_router)
                    .await
                    .ok()
                    .flatten();
                router = Some((Instant::now(), loaded));
            }
            let current = router.as_ref().and_then(|(_, router)| router.as_ref());
            tick_model_discovery(current).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_cache_lists_only_offered_slugs() {
        let body = json!({"models":[
            {"slug":"gpt-6.1-sol","visibility":"list"},
            {"slug":"gpt-reserve","visibility":"hide"},
            {"slug":"gpt-6-luna"}
        ]});
        assert_eq!(
            parse_codex_models_cache(&body),
            vec!["gpt-6.1-sol", "gpt-6-luna"]
        );
        assert!(parse_codex_models_cache(&json!({})).is_empty());
    }

    #[test]
    fn an_unsigned_cli_listing_is_a_failure_not_a_short_list() {
        let unsigned = "You are not authenticated.\n\nAvailable models:\n  * grok-4.6 (default)\n";
        assert!(listing_from_cli_output(unsigned).is_err());
        let signed = "Available models:\n  * grok-4.7 (default)\n  - grok-4.6\n";
        assert_eq!(
            listing_from_cli_output(signed).unwrap(),
            vec!["grok-4.7", "grok-4.6"]
        );
        assert!(listing_from_cli_output("Fetching available models...\n").is_err());
    }

    #[test]
    fn plane_cli_binaries_and_engines_map_to_their_sources() {
        assert_eq!(
            source_for_cli_binary(Path::new("/opt/bin/agy")),
            Some(SOURCE_AGY)
        );
        assert_eq!(
            source_for_cli_binary(Path::new("/opt/bin/claude")),
            Some(SOURCE_CLAUDE)
        );
        assert_eq!(source_for_cli_binary(Path::new("/opt/bin/pi")), None);
        assert_eq!(engine_source(CodingEngineKind::Pi), None);
        assert_eq!(engine_source(CodingEngineKind::AgyCli), Some(SOURCE_AGY));
        for (engine, _, source) in NATIVE_ENGINE_SOURCES {
            assert!(source.starts_with("harness-"), "{engine}");
        }
    }

    #[test]
    fn the_worker_is_wired_at_boot_and_never_from_a_request_handler() {
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        let compact: String = bin.split_whitespace().collect();
        assert!(compact.contains("::spawn_model_discovery_worker()"));
    }
}
