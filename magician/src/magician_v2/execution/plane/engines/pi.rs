//! Pi 0.87.1 as a Plane chat and agentic harness.
//!
//! Pi's coding RPC driver owns the process and `agent_settled` boundary. A
//! separate, explicit extension presents the grant-scoped Plane MCP catalog;
//! Pi's built-in and operator extensions are disabled for this posture.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::execution::coding_engine::pi::PiCodingEngineAdapter;
use crate::magician_v2::execution::coding_engine::{
    CodingDispatchNotice, CodingEngineAdapter, CodingEngineRequest,
};
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
use crate::magician_v2::execution::plane::engine::{
    revoke_session_grant, HarnessCapabilities, HarnessEngine, HarnessError, HarnessSession,
    HarnessSessionRequest, HarnessStopReason, HarnessStreamSink, HarnessTurnInput,
    HarnessTurnSettled, HarnessUsage, NativeToolPosture,
};
use crate::magician_v2::execution::plane::engines::oneshot::{seed_named_file, write_private};
use crate::magician_v2::execution::plane::grant::{plane_grant_registry, PlaneTurnStopReason};

const BRIDGE: &str = include_str!("../../../../../assets/pi-extensions/magician-plane.js");

#[derive(Debug, Clone)]
pub struct PiPlaneEngine {
    pub binary: PathBuf,
}

impl Default for PiPlaneEngine {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("pi"),
        }
    }
}

#[async_trait]
impl HarnessEngine for PiPlaneEngine {
    fn name(&self) -> &'static str {
        "pi"
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            supports_resume: true,
            tools_list_changed: true,
            streams_text_deltas: true,
            native_tool_posture: NativeToolPosture::Stripped,
        }
    }

    async fn start(
        &self,
        req: &HarnessSessionRequest,
    ) -> Result<Box<dyn HarnessSession>, HarnessError> {
        let turn_stop = plane_grant_registry()
            .resolve_run_scoped(&req.grant)
            .await
            .map(|grant| grant.turn_stop_signal());
        let owned = req.native_home.is_none();
        let home = req.native_home.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "magician-plane-pi-{}",
                uuid::Uuid::new_v4().simple()
            ))
        });
        let use_operator_model = !req
            .pi_profile
            .as_ref()
            .is_some_and(profile_uses_private_model);
        if let Err(error) = install_home(&home, &operator_pi_home(), use_operator_model) {
            // install_home may have copied auth.json before a later write
            // failed. Do not leave that copy in a failed temp session.
            if owned {
                let _ = std::fs::remove_dir_all(&home);
            }
            revoke_session_grant(&req.grant).await;
            return Err(HarnessError::Message(format!("Pi Plane home: {error}")));
        }
        let profile_route = match req.pi_profile.as_ref() {
            Some(profile) => match install_profile(&home, profile) {
                Ok(route) => Some(route),
                Err(error) => {
                    if owned {
                        let _ = std::fs::remove_dir_all(&home);
                    }
                    revoke_session_grant(&req.grant).await;
                    return Err(HarnessError::Message(format!("Pi profile: {error}")));
                },
            },
            None => None,
        };
        let session_dir = if req.planning_only {
            // Proposals have no resumable work session. Keep their transcript
            // in the temporary home that shutdown already removes.
            home.join("sessions")
        } else {
            req.native_home
                .as_ref()
                .map(|home| home.join("sessions"))
                .unwrap_or_else(agentic_sessions_root)
        };
        if let Err(error) = ensure_private_dir(&session_dir) {
            if owned {
                let _ = std::fs::remove_dir_all(&home);
            }
            revoke_session_grant(&req.grant).await;
            return Err(HarnessError::Message(format!("Pi Plane sessions: {error}")));
        }
        Ok(Box::new(PiPlaneSession {
            adapter: PiCodingEngineAdapter::new(runtime_core::process::resolve_program(
                self.binary.as_os_str(),
                None,
            )),
            endpoint: req.endpoint.url.clone(),
            grant: req.grant.clone(),
            system_prompt: req.system_prompt.clone(),
            provider: profile_route.as_ref().map(|route| route.provider.clone()),
            model: profile_route
                .as_ref()
                .map(|route| route.model.clone())
                .or_else(|| req.model.clone().filter(|model| model != "default")),
            thinking_level: profile_route
                .as_ref()
                .and_then(|route| route.thinking_level.clone()),
            images: req.pi_images.clone(),
            env_allowlist: {
                let mut keys = req.env_allowlist.clone();
                keys.extend(
                    provider_key_allowlist(req.pi_profile.as_ref())
                        .iter()
                        .map(|key| (*key).to_string()),
                );
                if let Some(key) = profile_route
                    .as_ref()
                    .and_then(|route| route.api_key_env.as_ref())
                {
                    keys.push(key.clone());
                }
                keys
            },
            cancel: req.cancel.clone(),
            turn_stop,
            timeout: req.turn_timeout,
            home,
            session_dir,
            owned,
            resume_id: req.resume_session_id.clone(),
            released: false,
            retain_grant: false,
        }))
    }
}

struct PiProfileRoute {
    provider: String,
    model: String,
    thinking_level: Option<String>,
    api_key_env: Option<String>,
}

const DEFAULT_PROVIDER_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GROQ_API_KEY",
];

fn provider_key_allowlist(
    profile: Option<&magicllm::config::LlmConfig>,
) -> &'static [&'static str] {
    let Some(profile) = profile else {
        // Pi's own default model may use any of its configured providers.
        return DEFAULT_PROVIDER_KEYS;
    };
    if profile_uses_private_model(profile) {
        // The selected profile's custom key is added separately; a keyless
        // local endpoint does not need a provider key at all.
        return &[];
    }
    match profile.provider.as_str() {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "gemini" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "groq" => &["GROQ_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        _ => &[],
    }
}

fn profile_uses_private_model(profile: &magicllm::config::LlmConfig) -> bool {
    profile
        .api_key_env
        .as_deref()
        .is_some_and(|key| !key.is_empty())
        || profile.api_base_url.is_some()
        || profile.provider.as_str() == "ollama"
}

fn install_profile(
    home: &Path,
    profile: &magicllm::config::LlmConfig,
) -> Result<PiProfileRoute, String> {
    let provider = profile.provider.as_str();
    let model = profile.model.trim();
    if model.is_empty() {
        return Err("model is empty".into());
    }
    let key_env = profile.api_key_env.as_deref().filter(|key| !key.is_empty());
    if let Some(key) = key_env {
        if !key
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("API key environment variable name is invalid".into());
        }
        if std::env::var_os(key).is_none() {
            return Err(format!(
                "{key} is not set in the Magician runtime environment"
            ));
        }
    }
    let (api, default_url) = match provider {
        "openai" => ("openai-responses", Some("https://api.openai.com/v1")),
        // Pi's Anthropic client appends `/v1/messages` to the base itself;
        // a base ending in `/v1` sent every turn to `/v1/v1/messages` and
        // Anthropic answered 404 `not_found_error` before the first token.
        "anthropic" => ("anthropic-messages", Some("https://api.anthropic.com")),
        "gemini" => (
            "google-generative-ai",
            Some("https://generativelanguage.googleapis.com/v1beta"),
        ),
        "openrouter" => ("openai-completions", Some("https://openrouter.ai/api/v1")),
        "deepseek" => ("openai-completions", Some("https://api.deepseek.com/v1")),
        "minimax" => (
            "anthropic-messages",
            Some("https://api.minimax.io/anthropic"),
        ),
        "xai" => ("openai-completions", Some("https://api.x.ai/v1")),
        "ollama" => ("openai-completions", Some("http://127.0.0.1:11434/v1")),
        _ => ("openai-completions", None),
    };
    let mut selected_provider = if provider == "gemini" {
        "google".to_string()
    } else {
        provider.to_string()
    };
    // Pi has no built-in Ollama model catalog. Even a plain Ollama profile
    // needs a private compatible-provider entry at the default local URL.
    if profile_uses_private_model(profile) {
        let mut base_url = profile
            .api_base_url
            .as_deref()
            .or(default_url)
            .ok_or_else(|| format!("provider '{provider}' needs an API base URL for Pi"))?
            .to_string();
        if provider == "ollama" {
            if let Some(endpoint_base) = base_url.strip_suffix("/api/generate") {
                base_url = format!("{endpoint_base}/v1");
            }
        }
        if api == "anthropic-messages" {
            if let Some(root) = base_url.trim_end_matches('/').strip_suffix("/v1") {
                base_url = root.to_string();
            }
        }
        let local_endpoint = url::Url::parse(&base_url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"));
        if key_env.is_none() && provider != "ollama" && !local_endpoint {
            return Err("custom API endpoints need an API key environment variable".into());
        }
        selected_provider = "magician-plane-profile".into();
        let models_path = home.join("agent").join("models.json");
        let mut models: serde_json::Value = std::fs::read(&models_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| serde_json::json!({"providers": {}}));
        if !models
            .get("providers")
            .is_some_and(serde_json::Value::is_object)
        {
            models["providers"] = serde_json::json!({});
        }
        let mut model_entry = serde_json::json!({
            "id": model,
            "reasoning": profile.supports_reasoning.unwrap_or(profile.reasoning_effort.is_some()),
            "input": if crate::magician_v2::query_analysis::multi_llm_service::MultiLLMService::chat_profile_supports_user_image_inputs(profile) { vec!["text", "image"] } else { vec!["text"] },
            "maxTokens": profile.max_tokens.filter(|tokens| *tokens > 0).unwrap_or(16_384),
        });
        // The pinned Pi catalog predates GPT-6.1 Sol. Describe this model
        // explicitly so its private route cannot inherit zero cost, a 128K
        // context, or unsupported off/minimal thinking levels.
        if provider == "openai" && (model == "gpt-6.1-sol" || model.starts_with("gpt-6.1-sol-")) {
            model_entry["contextWindow"] = serde_json::json!(1_050_000);
            model_entry["maxTokens"] = serde_json::json!(profile
                .max_tokens
                .filter(|tokens| *tokens > 0)
                .unwrap_or(128_000)
                .min(128_000));
            model_entry["reasoning"] = serde_json::json!(true);
            model_entry["thinkingLevelMap"] = serde_json::json!({
                "off": null, "minimal": null, "low": "low", "medium": "medium",
                "high": "high", "xhigh": "xhigh", "max": "max"
            });
            model_entry["cost"] = serde_json::json!({
                "input": 2.0, "output": 10.0, "cacheRead": 0.10, "cacheWrite": 2.50,
                "tiers": [{ "inputTokensAbove": 272_000, "input": 4.0,
                    "output": 15.0, "cacheRead": 0.20, "cacheWrite": 5.0 }]
            });
        }
        // Pi sends `thinking.type = enabled` with a token budget unless the
        // model is marked adaptive; Opus 5.x, Sonnet 5 and Opus 4.7 refuse
        // that with a 400 before the first token. The rule is Magician's own
        // Anthropic client's, so both call the model the same way.
        if api == "anthropic-messages"
            && magicllm::providers::anthropic_messages::anthropic_model_uses_adaptive_thinking(
                model,
            )
        {
            model_entry["compat"] = serde_json::json!({"forceAdaptiveThinking": true});
        }
        models["providers"][&selected_provider] = serde_json::json!({
            "baseUrl": base_url,
            "api": api,
            "apiKey": key_env.map(|key| format!("${key}")).unwrap_or_else(|| "local".into()),
            "models": [model_entry],
        });
        write_private(&models_path, models.to_string().as_bytes())
            .map_err(|error| error.to_string())?;
    }
    Ok(PiProfileRoute {
        provider: selected_provider,
        model: model.to_string(),
        thinking_level: profile
            .reasoning_effort
            .as_deref()
            .and_then(|effort| match effort {
                "default" | "" => None,
                "none" => Some("off".to_string()),
                other => Some(other.to_string()),
            }),
        api_key_env: key_env.map(str::to_string),
    })
}

fn operator_pi_home() -> PathBuf {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".pi")
                .join("agent")
        })
}

fn agentic_sessions_root() -> PathBuf {
    if crate::magician_v2::artifact_v2::workspace::running_under_cargo_test_harness() {
        std::env::temp_dir()
            .join(format!("magician-cargo-test-{}", std::process::id()))
            .join("plane-pi-agentic-sessions")
    } else {
        crate::magician_v2::process_storage::runtime_root()
            .join(".magician-storage")
            .join("plane")
            .join("pi-agentic-sessions")
    }
}

fn install_home(home: &Path, source: &Path, use_operator_model: bool) -> std::io::Result<()> {
    ensure_private_dir(home)?;
    let agent_dir = home.join("agent");
    ensure_private_dir(&agent_dir)?;
    if use_operator_model {
        // A resumed chat home may hold Pi's refreshed OAuth token. Do not
        // replace it with the older operator copy on the next turn.
        if !agent_dir.join("auth.json").exists() {
            seed_named_file(source, &agent_dir, "auth.json")?;
        }
        seed_named_file(source, &agent_dir, "models.json")?;
    }
    // Keep Pi's chosen default model without importing its package, tool,
    // extension, and project-trust settings into the governed Plane process.
    if let Ok(bytes) = std::fs::read(source.join("settings.json")) {
        if let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let mut defaults = serde_json::Map::new();
            for key in ["defaultProvider", "defaultModel", "defaultThinkingLevel"] {
                if let Some(value) = settings.get(key) {
                    defaults.insert(key.to_string(), value.clone());
                }
            }
            if !defaults.is_empty() {
                write_private(
                    &agent_dir.join("settings.json"),
                    serde_json::Value::Object(defaults).to_string().as_bytes(),
                )?;
            }
        }
    }
    write_private(&home.join("magician-plane.js"), BRIDGE.as_bytes())?;
    Ok(())
}

fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

struct PiPlaneSession {
    adapter: PiCodingEngineAdapter,
    endpoint: String,
    grant: String,
    system_prompt: String,
    model: Option<String>,
    provider: Option<String>,
    thinking_level: Option<String>,
    images: Vec<serde_json::Value>,
    env_allowlist: Vec<String>,
    cancel: Option<CancellationToken>,
    turn_stop: Option<CancellationToken>,
    timeout: Duration,
    home: PathBuf,
    /// Chat keeps sessions in its conversation home; agentic turns use a
    /// private shared directory so each iteration can resume its native id.
    session_dir: PathBuf,
    owned: bool,
    resume_id: Option<String>,
    released: bool,
    retain_grant: bool,
}

impl PiPlaneSession {
    fn session_id(&self) -> Option<String> {
        self.resume_id.clone().filter(|id| !id.is_empty())
    }

    fn clean_files(&self) {
        for name in ["bridge.json", "bridge.ready", "system.md"] {
            let _ = std::fs::remove_file(self.home.join(name));
        }
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    async fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.clean_files();
        if !self.retain_grant {
            revoke_session_grant(&self.grant).await;
        }
    }

    async fn stop_reason(&self) -> Option<PlaneTurnStopReason> {
        plane_grant_registry()
            .resolve_run_scoped(&self.grant)
            .await
            .and_then(|grant| grant.turn_stop_reason())
    }

    fn request(
        &self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
        cancel: CancellationToken,
        live_id: Arc<Mutex<Option<String>>>,
    ) -> CodingEngineRequest {
        let mut prompt = input.text.clone();
        for steer in &input.operator_steer {
            prompt.push_str("\n\nOperator update: ");
            prompt.push_str(steer);
        }
        // Agentic turns discard their credential-bearing temp home after
        // each iteration. Pi 0.87.1 filters --session lookup by cwd even
        // with --session-dir; a fresh temp cwd makes the prior session look
        // foreign and prompts for an interactive fork. Use the private shared
        // session directory as the stable agentic cwd.
        let working_dir = if self.owned {
            self.session_dir.clone()
        } else {
            self.home.clone()
        };
        let mut request = CodingEngineRequest::new(
            prompt,
            self.home.clone(),
            self.home.clone(),
            working_dir.clone(),
            TransactionScope {
                principal: "plane".into(),
                workspace: "plane".into(),
            },
        );
        request.stage_result = false;
        request.working_dir = Some(working_dir);
        request.pi.plane_harness = true;
        request.pi.session_dir = Some(self.session_dir.clone());
        request.pi.resume_session_id = self.resume_id.clone();
        request.pi.model = self.model.clone();
        request.pi.provider = self.provider.clone();
        request.pi.thinking_level = self.thinking_level.clone();
        request.pi.images = self.images.clone();
        request.pi.system_prompt_path = Some(self.home.join("system.md"));
        request.pi.extension_paths = vec![self.home.join("magician-plane.js")];
        request.pi.ready_marker = Some(self.home.join("bridge.ready"));
        request.cancel_token = Some(cancel);
        request.timeout = if self.timeout.is_zero() {
            Duration::from_secs(365 * 24 * 60 * 60)
        } else {
            self.timeout
        };
        request.env = plane_env(&self.home, &self.env_allowlist);
        request.event_sink = Some(Arc::new({
            let sink = sink.clone();
            move |event| {
                if let Some(delta) = &event.text_delta {
                    sink.emit(delta);
                }
            }
        }));
        request.dispatch_sink = Some(Arc::new(move |notice| {
            if let CodingDispatchNotice::LiveSession(reference) = notice {
                if let Ok(mut slot) = live_id.lock() {
                    *slot = Some(reference.native_session_id);
                }
            }
        }));
        request
    }
}

fn pi_stop_reason(
    stop: Option<PlaneTurnStopReason>,
    externally_cancelled: bool,
    succeeded: bool,
    timed_out: bool,
) -> HarnessStopReason {
    // Stop is an operator decision for the whole run. An approval/delegation
    // latch may already be set when Stop arrives during process teardown.
    if externally_cancelled {
        return HarnessStopReason::Cancelled;
    }
    match stop {
        Some(PlaneTurnStopReason::NeedsApproval) => HarnessStopReason::NeedsApproval,
        Some(PlaneTurnStopReason::Delegate) => HarnessStopReason::Delegate,
        Some(PlaneTurnStopReason::TurnBudgetSpent) => HarnessStopReason::TurnBudgetSpent,
        None if succeeded => HarnessStopReason::Settled,
        None if timed_out => HarnessStopReason::TurnBudgetSpent,
        None => HarnessStopReason::Refused,
    }
}

fn plane_env(home: &Path, allowlist: &[String]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for key in ["PATH", "HOME", "USER", "TMPDIR", "LANG"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    for key in allowlist {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.clone(), value);
        }
    }
    env.insert(
        "PI_CODING_AGENT_DIR".into(),
        home.join("agent").display().to_string(),
    );
    env.insert(
        "MAGICIAN_PI_PLANE_CONFIG".into(),
        home.join("bridge.json").display().to_string(),
    );
    env.insert(
        "MAGICIAN_PI_PLANE_READY".into(),
        home.join("bridge.ready").display().to_string(),
    );
    env
}

#[async_trait]
impl HarnessSession for PiPlaneSession {
    async fn turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError> {
        if self.released
            || self
                .cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        {
            self.release().await;
            return Ok(HarnessTurnSettled {
                assistant_text: String::new(),
                stop_reason: HarnessStopReason::Cancelled,
                usage: None,
                native_session_id: self.session_id(),
            });
        }
        // Each adapter run opens one RPC process, so both files are refreshed
        // before this turn and the extension must prove it registered tools.
        let _ = std::fs::remove_file(self.home.join("bridge.ready"));
        let config = serde_json::json!({ "url": self.endpoint, "grant": self.grant });
        if let Err(error) = write_private(
            &self.home.join("bridge.json"),
            config.to_string().as_bytes(),
        ) {
            self.release().await;
            return Err(HarnessError::Message(format!(
                "Pi Plane bridge config: {error}"
            )));
        }
        if let Err(error) =
            write_private(&self.home.join("system.md"), self.system_prompt.as_bytes())
        {
            self.release().await;
            return Err(HarnessError::Message(format!(
                "Pi Plane system prompt: {error}"
            )));
        }

        let turn_cancel = CancellationToken::new();
        let watch_cancel = turn_cancel.clone();
        let external = self.cancel.clone();
        let turn_stop = self.turn_stop.clone();
        let watcher = tokio::spawn(async move {
            tokio::select! {
                _ = async { if let Some(token) = external { token.cancelled().await } else { std::future::pending().await } } => {},
                _ = async { if let Some(token) = turn_stop { token.cancelled().await } else { std::future::pending().await } } => {},
            }
            watch_cancel.cancel();
        });
        let live_id = Arc::new(Mutex::new(None));
        let usage = Arc::new(Mutex::new(None));
        let mut request = self.request(input, sink, turn_cancel, live_id.clone());
        request.usage_capture = Some(usage.clone());
        let outcome = self.adapter.run_turn(request).await;
        watcher.abort();
        if let Ok(slot) = live_id.lock() {
            if let Some(id) = slot.clone() {
                self.resume_id = Some(id);
            } else if outcome.is_err() {
                // A seeded resume id that Pi never acknowledged must not be
                // offered again on the next turn.
                self.resume_id = None;
            }
        }
        let stop = self.stop_reason().await;
        let stop_reason = pi_stop_reason(
            stop,
            self.cancel
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled),
            outcome.is_ok(),
            outcome
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("timed out")),
        );
        if matches!(
            stop_reason,
            HarnessStopReason::NeedsApproval | HarnessStopReason::Delegate
        ) {
            self.retain_grant = true;
        }
        if let Ok(result) = &outcome {
            if let Some(id) = &result.session_id {
                self.resume_id = Some(id.clone());
            }
        }
        let native_session_id = self.session_id();
        let assistant_text = if matches!(
            stop_reason,
            HarnessStopReason::Settled | HarnessStopReason::Refused
        ) {
            outcome
                .as_ref()
                .ok()
                .and_then(|r| r.assistant_text.clone())
                .unwrap_or_else(|| {
                    outcome
                        .as_ref()
                        .err()
                        .map(|e| format!("Pi turn failed: {e}"))
                        .unwrap_or_default()
                })
        } else {
            String::new()
        };
        let usage = usage.lock().ok().and_then(|slot| {
            // Pi reports uncached input, cache reads and cache writes apart;
            // `input` alone undercounted a cached turn to a few tokens.
            slot.as_ref().map(|spent| HarnessUsage {
                input_tokens: spent
                    .input
                    .saturating_add(spent.cache_read)
                    .saturating_add(spent.cache_write),
                cached_input_tokens: spent.cache_read,
                cache_read_reported: true,
                cache_creation_tokens: Some(spent.cache_write),
                cost_usd: spent.cost_known.then_some(spent.cost).filter(|cost| {
                    cost.is_finite()
                            && *cost >= 0.0
                            // Our private Pi model entry supplies no pricing
                            // table. Pi's default zero is not a free-call fact.
                            && (*cost > 0.0
                                || self.provider.as_deref() != Some("magician-plane-profile"))
                }),
                model: self.model.clone(),
                output_tokens: spent.output,
            })
        });
        self.release().await;
        Ok(HarnessTurnSettled {
            assistant_text,
            stop_reason,
            usage,
            native_session_id,
        })
    }

    async fn shutdown(&mut self) {
        self.release().await;
    }
}

impl Drop for PiPlaneSession {
    fn drop(&mut self) {
        if !self.released {
            self.clean_files();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agentic_request_uses_session_directory_as_stable_cwd() {
        let root = tempfile::tempdir().expect("temp root");
        let home = root.path().join("temporary-home");
        let session_dir = root.path().join("sessions");
        std::fs::create_dir_all(&home).expect("temp home");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        let session = PiPlaneSession {
            adapter: PiCodingEngineAdapter::default(),
            endpoint: String::new(),
            grant: String::new(),
            system_prompt: String::new(),
            model: None,
            provider: None,
            thinking_level: None,
            images: vec![serde_json::json!({
                "type": "image",
                "data": "cGl4ZWxz",
                "mimeType": "image/png"
            })],
            env_allowlist: Vec::new(),
            cancel: None,
            turn_stop: None,
            timeout: Duration::from_secs(1),
            home,
            session_dir: session_dir.clone(),
            owned: true,
            resume_id: None,
            released: false,
            retain_grant: false,
        };
        let request = session.request(
            &HarnessTurnInput {
                text: "Continue".into(),
                operator_steer: Vec::new(),
            },
            &HarnessStreamSink::drain(),
            CancellationToken::new(),
            Arc::new(Mutex::new(None)),
        );
        assert_eq!(request.working_dir.as_deref(), Some(session_dir.as_path()));
        assert_eq!(request.scope_root, session_dir);
        assert_eq!(request.pi.images.len(), 1);
        assert_eq!(request.pi.images[0]["mimeType"], "image/png");
    }

    #[test]
    fn gpt_6_1_sol_pi_profile_has_explicit_catalog_contract() {
        let home = tempfile::tempdir().expect("temp home");
        std::env::set_var("MAGICIAN_PI_TEST_SOL_KEY", "test");
        let profile = magicllm::config::LlmConfig {
            provider: magicllm::capability::LLMProviderKind::OpenAI,
            model: "gpt-6.1-sol".into(),
            api_key_env: Some("MAGICIAN_PI_TEST_SOL_KEY".into()),
            reasoning_effort: Some("high".into()),
            max_tokens: Some(65_536),
            ..Default::default()
        };
        let route = install_profile(home.path(), &profile).expect("Sol profile");
        assert_eq!(route.model, "gpt-6.1-sol");
        assert_eq!(route.thinking_level.as_deref(), Some("high"));
        let models: serde_json::Value =
            serde_json::from_slice(&std::fs::read(home.path().join("agent/models.json")).unwrap())
                .unwrap();
        let provider = &models["providers"]["magician-plane-profile"];
        assert_eq!(provider["api"], "openai-responses");
        let model = &provider["models"][0];
        assert_eq!(model["contextWindow"], 1_050_000);
        assert_eq!(model["maxTokens"], 65_536);
        assert!(model["thinkingLevelMap"]["off"].is_null());
        assert!(model["thinkingLevelMap"]["minimal"].is_null());
        assert_eq!(model["thinkingLevelMap"]["low"], "low");
        assert_eq!(model["cost"]["cacheRead"], 0.10);
        assert_eq!(model["cost"]["tiers"][0]["inputTokensAbove"], 272_000);
    }

    #[test]
    fn ollama_profile_installs_a_compatible_local_model() {
        let home = tempfile::tempdir().expect("temp home");
        let profile = magicllm::config::LlmConfig {
            provider: magicllm::capability::LLMProviderKind::Ollama,
            model: "qwen2.5-coder:7b".into(),
            api_key_env: None,
            api_base_url: None,
            ..Default::default()
        };
        let route = install_profile(home.path(), &profile).expect("Ollama profile");
        assert_eq!(route.provider, "magician-plane-profile");
        let models: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.path().join("agent/models.json")).expect("models.json"),
        )
        .expect("valid models.json");
        assert_eq!(
            models["providers"]["magician-plane-profile"]["baseUrl"],
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            models["providers"]["magician-plane-profile"]["models"][0]["id"],
            "qwen2.5-coder:7b"
        );
    }

    #[test]
    fn anthropic_profiles_give_pi_the_api_root_it_appends_v1_messages_to() {
        std::env::set_var("MAGICIAN_PI_TEST_ANTHROPIC_KEY", "test");
        for (configured, expected) in [
            (None, "https://api.anthropic.com"),
            (
                Some("https://api.anthropic.com/v1"),
                "https://api.anthropic.com",
            ),
            (Some("https://proxy.example/v1/"), "https://proxy.example"),
        ] {
            let home = tempfile::tempdir().expect("temp home");
            let profile = magicllm::config::LlmConfig {
                provider: magicllm::capability::LLMProviderKind::Anthropic,
                model: "claude-opus-5-5".into(),
                api_key_env: Some("MAGICIAN_PI_TEST_ANTHROPIC_KEY".into()),
                api_base_url: configured.map(str::to_string),
                ..Default::default()
            };
            install_profile(home.path(), &profile).expect("Anthropic profile");
            let models: serde_json::Value = serde_json::from_slice(
                &std::fs::read(home.path().join("agent/models.json")).expect("models.json"),
            )
            .expect("valid models.json");
            assert_eq!(
                models["providers"]["magician-plane-profile"]["baseUrl"],
                expected
            );
            // Opus 5.5 refuses a thinking budget; Pi must send adaptive.
            assert_eq!(
                models["providers"]["magician-plane-profile"]["models"][0]["compat"]
                    ["forceAdaptiveThinking"],
                true
            );
        }
    }

    #[test]
    fn pi_profile_infers_image_input_the_same_way_as_chat_picker() {
        let home = tempfile::tempdir().expect("temp home");
        let profile = magicllm::config::LlmConfig {
            provider: magicllm::capability::LLMProviderKind::Anthropic,
            model: "claude-test-vision".into(),
            api_key_env: None,
            api_base_url: Some("http://127.0.0.1:8899/v1".into()),
            supports_vision: None,
            ..Default::default()
        };
        install_profile(home.path(), &profile).expect("Anthropic profile");
        let models: serde_json::Value = serde_json::from_slice(
            &std::fs::read(home.path().join("agent/models.json")).expect("models.json"),
        )
        .expect("valid models.json");
        assert_eq!(
            models["providers"]["magician-plane-profile"]["models"][0]["input"],
            serde_json::json!(["text", "image"])
        );
    }

    #[test]
    fn explicit_pi_profile_limits_provider_environment_keys() {
        let anthropic = magicllm::config::LlmConfig {
            provider: magicllm::capability::LLMProviderKind::Anthropic,
            api_key_env: None,
            ..Default::default()
        };
        assert_eq!(
            provider_key_allowlist(Some(&anthropic)),
            &["ANTHROPIC_API_KEY"]
        );
        assert!(!provider_key_allowlist(Some(&anthropic)).contains(&"OPENAI_API_KEY"));

        let custom = magicllm::config::LlmConfig {
            api_key_env: Some("CUSTOM_API_KEY".into()),
            ..Default::default()
        };
        assert!(provider_key_allowlist(Some(&custom)).is_empty());
        let local = magicllm::config::LlmConfig {
            api_key_env: None,
            api_base_url: Some("http://127.0.0.1:8899/v1".into()),
            ..Default::default()
        };
        assert!(provider_key_allowlist(Some(&local)).is_empty());
        assert!(provider_key_allowlist(None).contains(&"OPENAI_API_KEY"));
    }

    #[test]
    fn private_pi_profile_home_does_not_copy_operator_credentials() {
        let source = tempfile::tempdir().expect("operator Pi home");
        write_private(&source.path().join("auth.json"), b"operator-oauth-token")
            .expect("operator auth");
        write_private(&source.path().join("models.json"), b"operator-provider-key")
            .expect("operator models");
        let private = tempfile::tempdir().expect("private Pi home");
        install_home(private.path(), source.path(), false).expect("private profile setup");
        assert!(!private.path().join("agent/auth.json").exists());
        assert!(!private.path().join("agent/models.json").exists());

        let profile = magicllm::config::LlmConfig {
            provider: magicllm::capability::LLMProviderKind::Ollama,
            model: "local".into(),
            api_key_env: None,
            ..Default::default()
        };
        assert!(profile_uses_private_model(&profile));
        install_profile(private.path(), &profile).expect("private model entry");
        let models = std::fs::read_to_string(private.path().join("agent/models.json"))
            .expect("private models");
        assert!(models.contains("magician-plane-profile"));
        assert!(!models.contains("operator-provider-key"));

        let inherited = tempfile::tempdir().expect("default Pi home");
        install_home(inherited.path(), source.path(), true).expect("operator default setup");
        assert!(inherited.path().join("agent/auth.json").exists());
        assert!(inherited.path().join("agent/models.json").exists());
    }

    #[test]
    fn parent_stop_wins_a_concurrent_plane_pause() {
        assert_eq!(
            pi_stop_reason(Some(PlaneTurnStopReason::NeedsApproval), true, false, false),
            HarnessStopReason::Cancelled
        );
        assert_eq!(
            pi_stop_reason(Some(PlaneTurnStopReason::Delegate), true, false, false),
            HarnessStopReason::Cancelled
        );
        assert_eq!(
            pi_stop_reason(
                Some(PlaneTurnStopReason::NeedsApproval),
                false,
                false,
                false
            ),
            HarnessStopReason::NeedsApproval
        );
    }
}
