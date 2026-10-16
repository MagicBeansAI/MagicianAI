//! System One HTTP adapter — every model that speaks `POST /v1/systemone`.
//!
//! TypeSafe's hosted Jev defined the dialect; the open Jev-style models
//! (laya, Kev, edgejev) serve the same request and response shapes, so one
//! adapter covers all of them. What differs per model is a profile: the
//! endpoint, whether a bearer key is sent, the identity label, and the
//! capabilities the model honestly declares (plan Part IV, milestone E2).
//!
//! Owns the wire format, auth, retries, and the model id — nothing else.
//! The request maps the Magician IR onto `POST {endpoint}` and the response
//! maps back 1:1; answers are validated against the request's declared
//! options on the way in so an undeclared option surfaces here, at the
//! adapter boundary, as [`DecisionError::UnknownOption`].
//!
//! Posture copied from the magicllm provider rules: no-redirect client
//! (redirects would move the attested endpoint identity) and a bounded
//! response body read. `default_http_client` there is `pub(crate)`, so this
//! builds its own.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::primitives::{Criteria, Instruction, Question};
use crate::request::{validate_answers, Answer, DecisionRequest, DecisionResponse, Usage};

/// Response bodies are capped at 1 MiB: answers are small structured JSON,
/// and an unbounded read turns a misbehaving endpoint into a memory fault.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug, Clone)]
pub struct SystemOneConfig {
    /// Identity label echoed on every response (`typesafe` for hosted Jev,
    /// `systemone` for a self-hosted model). Thresholds key on it.
    pub adapter: String,
    /// Full URL, e.g. `https://api.typesafe.ai/v1/systemone`.
    pub endpoint: String,
    /// Resolved API key, sent as a bearer token when present. A local
    /// model server takes none. The engine reads the env var named by
    /// `api_key_env` — the adapter never touches the environment.
    pub api_key: Option<String>,
    /// Model id to request, e.g. `jev-latest` or a pinned `jev-1.13.0`.
    /// Pin when thresholds are tuned; the response's echoed model is
    /// checked against this and a mismatch is logged.
    pub model: String,
    pub timeout: Duration,
    /// Extra attempts beyond the first, for 429/529/5xx/timeout only.
    pub max_retries: u32,
    pub capabilities: ModelCapabilities,
}

impl SystemOneConfig {
    /// The hosted-Jev profile: keyed, calibrated, 255-option Choice.
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            adapter: "typesafe".to_string(),
            endpoint: endpoint.into(),
            api_key: Some(api_key.into()),
            model: model.into(),
            timeout: Duration::from_secs(15),
            max_retries: 2,
            capabilities: typesafe_capabilities(),
        }
    }

    /// A self-hosted model: no key, and nothing claimed beyond the
    /// conservative defaults until the operator's profile declares it.
    pub fn keyless(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let endpoint = endpoint.into();
        Self {
            adapter: "systemone".to_string(),
            capabilities: ModelCapabilities {
                remote: !is_loopback_endpoint(&endpoint),
                ..ModelCapabilities::default()
            },
            endpoint,
            api_key: None,
            model: model.into(),
            timeout: Duration::from_secs(15),
            max_retries: 2,
        }
    }
}

/// What hosted Jev honestly promises: trained-calibrated probabilities,
/// Choice capped at 255 options, Score supported, and it is remote.
pub fn typesafe_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        calibrated: true,
        text_only: true,
        max_state_tokens: Some(32_768),
        max_choice_options: Some(255),
        max_questions: None,
        supports_score: true,
        remote: true,
    }
}

/// Only a loopback host counts as local. Derived from the URL, never
/// declared, so a config typo cannot relabel a remote endpoint as local
/// and route body-seeing state off the box.
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

pub struct SystemOneDecisionModel {
    config: SystemOneConfig,
    client: reqwest::Client,
}

impl SystemOneDecisionModel {
    pub fn new(config: SystemOneConfig) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self::with_client(config, client)
    }

    /// Injection point for frozen-fixture tests (wiremock server URI).
    pub fn with_client(config: SystemOneConfig, client: reqwest::Client) -> Self {
        Self { config, client }
    }
}

// ---- wire types (System One dialect; never leaked past this file) ----

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    state: &'a serde_json::Value,
    questions: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    answers: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default, alias = "cache_creation_tokens")]
    cache_write_tokens: Option<u64>,
}

pub(crate) fn criteria_text(criteria: &Criteria) -> String {
    match criteria {
        Criteria::Str(text) => text.clone(),
        Criteria::Contrastive {
            what,
            not_for,
            examples,
        } => {
            let mut text = what.clone();
            if let Some(not_for) = not_for {
                text.push_str(&format!(" Not for: {not_for}"));
            }
            if !examples.is_empty() {
                text.push_str(&format!(" e.g. {}", examples.join("; ")));
            }
            text
        },
    }
}

fn instructions_text(instructions: &Instruction) -> String {
    instructions.as_text()
}

fn map_request(request: &DecisionRequest) -> serde_json::Map<String, serde_json::Value> {
    let mut questions = serde_json::Map::new();
    for question in &request.questions {
        let value = match question {
            Question::Choice(q) => {
                let criteria: serde_json::Map<String, serde_json::Value> = q
                    .criteria
                    .iter()
                    .map(|(option, criteria)| {
                        (
                            option.as_str().to_string(),
                            serde_json::Value::String(criteria_text(criteria)),
                        )
                    })
                    .collect();
                serde_json::json!({
                    "type": "choice",
                    "instructions": instructions_text(&q.instructions),
                    "criteria": criteria,
                })
            },
            Question::Score(q) => {
                serde_json::json!({
                    "type": "score",
                    "instructions": instructions_text(&q.instructions),
                    "criteria": q.levels.iter().map(criteria_text).collect::<Vec<_>>(),
                })
            },
            Question::Noul(q) => {
                // The reference dialect takes instructions only for a noul;
                // contrastive criteria fold into the instruction text.
                let mut text = instructions_text(&q.instructions);
                if let Some(criteria) = &q.criteria {
                    text.push_str(&format!(" True when: {}", criteria.is_true));
                    if let Some(is_false) = &criteria.is_false {
                        text.push_str(&format!(". Not true when: {is_false}"));
                    }
                }
                serde_json::json!({
                    "type": "noul",
                    "instructions": text,
                })
            },
        };
        questions.insert(question.id().as_str().to_string(), value);
    }
    questions
}

fn map_answer(question: &Question, raw: &serde_json::Value) -> Result<Answer, DecisionError> {
    let invalid = |detail: String| {
        DecisionError::InvalidResponse(format!("answer for '{}': {detail}", question.id().as_str()))
    };
    match question {
        Question::Choice(_) => {
            let choice = raw
                .get("choice")
                .and_then(|v| v.as_str())
                .ok_or_else(|| invalid("missing 'choice'".into()))?;
            let mut probabilities = std::collections::BTreeMap::new();
            if let Some(map) = raw.get("probabilities").and_then(|v| v.as_object()) {
                for (option, probability) in map {
                    let probability = probability.as_f64().ok_or_else(|| {
                        invalid(format!("non-numeric probability for '{option}'"))
                    })?;
                    probabilities.insert(crate::primitives::OptionId::new(option), probability);
                }
            }
            let confidence = raw
                .get("confidence")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| invalid("missing 'confidence'".into()))?;
            Ok(Answer::Choice {
                choice: crate::primitives::OptionId::new(choice),
                probabilities,
                confidence,
            })
        },
        Question::Score(q) => {
            let score = raw
                .get("score")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| invalid("missing 'score'".into()))?;
            let mut probabilities = vec![0.0; q.levels.len()];
            if let Some(map) = raw.get("probabilities").and_then(|v| v.as_object()) {
                for (level, probability) in map {
                    let index: usize = level
                        .parse()
                        .map_err(|_| invalid(format!("non-numeric score level '{level}'")))?;
                    let probability = probability.as_f64().ok_or_else(|| {
                        invalid(format!("non-numeric probability for level {index}"))
                    })?;
                    if index < probabilities.len() {
                        probabilities[index] = probability;
                    }
                }
            }
            let confidence = raw
                .get("confidence")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| invalid("missing 'confidence'".into()))?;
            Ok(Answer::Score {
                score,
                probabilities,
                confidence,
            })
        },
        Question::Noul(_) => {
            let noul = raw
                .get("noul")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| invalid("missing 'noul'".to_string()))?;
            Ok(Answer::Noul { noul })
        },
    }
}

#[async_trait]
impl StructuredDecisionModel for SystemOneDecisionModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new(&self.config.adapter, &self.config.model)
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.config.capabilities.clone()
    }

    fn records_attempts(&self) -> bool {
        true
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let wire = WireRequest {
            model: &self.config.model,
            state: request.state.as_json(),
            questions: map_request(&request),
        };
        let body = serde_json::to_vec(&wire)
            .map_err(|err| DecisionError::Transport(format!("request serialize failed: {err}")))?;
        let group = crate::telemetry::group_id();
        for index in 0..=self.config.max_retries {
            let mut receipt = crate::telemetry::Attempt::new(
                &request.operation,
                self.identity(),
                self.pricing_provider(),
                false,
                &group,
                index + 1,
            );
            let mut retry_after = None;
            let result = self
                .evaluate_once(&request, body.clone(), &mut receipt, &mut retry_after)
                .await;
            match &result {
                Ok(_) => receipt.succeeded(),
                Err(error) => receipt.failed(error),
            }
            // Persist this attempt before sleeping or starting another one.
            drop(receipt);
            let retryable = matches!(
                &result,
                Err(DecisionError::Timeout
                    | DecisionError::Transport(_)
                    | DecisionError::RateLimited { .. })
                    | Err(DecisionError::ProviderStatus {
                        status: 500..=599,
                        ..
                    })
            );
            if !retryable || index == self.config.max_retries {
                return result;
            }
            tokio::time::sleep(backoff_delay(index + 1, retry_after)).await;
        }
        unreachable!("at least one attempt")
    }
}

impl SystemOneDecisionModel {
    fn pricing_provider(&self) -> String {
        let hosted = reqwest::Url::parse(&self.config.endpoint).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some("api.typesafe.ai")
                && url.port_or_known_default() == Some(443)
                && url.path() == "/v1/systemone"
                && url.username().is_empty()
                && url.password().is_none()
        });
        if hosted {
            "decision:typesafe".into()
        } else {
            format!("decision:systemone:{}", self.config.adapter)
        }
    }

    async fn evaluate_once(
        &self,
        request: &DecisionRequest,
        body: Vec<u8>,
        receipt: &mut crate::telemetry::Attempt,
        attempt_retry_after: &mut Option<Duration>,
    ) -> Result<DecisionResponse, DecisionError> {
        let mut builder = self.client.post(&self.config.endpoint);
        if let Some(api_key) = &self.config.api_key {
            builder = builder.bearer_auth(api_key);
        }
        let response = builder
            .timeout(self.config.timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    DecisionError::Timeout
                } else {
                    DecisionError::Transport(err.to_string())
                }
            })?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        *attempt_retry_after = retry_after;
        let body = read_bounded(response).await?;
        // Even a billed response rejected by validation owns its usage.
        let decoded = serde_json::from_str::<WireResponse>(&body);
        if let Ok(wire) = &decoded {
            if let Some(model) = &wire.model {
                receipt.receipt.model = model.clone();
            }
            if let Some(usage) = &wire.usage {
                receipt.receipt.input_tokens = usage.input_tokens;
                receipt.receipt.output_tokens = usage.output_tokens;
                receipt.receipt.cache_read_tokens = usage.cache_read_tokens;
                receipt.receipt.cache_write_tokens = usage.cache_write_tokens;
            }
        }
        if !(200..300).contains(&status) {
            let error = DecisionError::ProviderStatus { status, body };
            if status == 429 && error.health_reason() != Some("structured_provider_credit") {
                return Err(DecisionError::RateLimited { retry_after });
            }
            return Err(error);
        }
        let wire = decoded.map_err(|err| {
            DecisionError::InvalidResponse(format!("response parse failed: {err}"))
        })?;
        if let Some(echoed) = &wire.model {
            if echoed != &self.config.model {
                tracing::warn!(requested = %self.config.model, answered = %echoed,
                    adapter = %self.config.adapter, "systemone adapter: response model differs from the requested pin");
            }
        }
        let mut answers = std::collections::BTreeMap::new();
        for question in &request.questions {
            let raw = wire.answers.get(question.id().as_str()).ok_or_else(|| {
                DecisionError::InvalidResponse(format!(
                    "response missing answer for '{}'",
                    question.id().as_str()
                ))
            })?;
            answers.insert(question.id().clone(), map_answer(question, raw)?);
        }
        let decision_response = DecisionResponse {
            model: ModelIdentity::new(
                &self.config.adapter,
                wire.model.unwrap_or_else(|| self.config.model.clone()),
            ),
            pack_id: request.pack_id.clone(),
            pack_version: request.pack_version.clone(),
            answers,
            usage: Usage {
                input_tokens: receipt.receipt.input_tokens.unwrap_or(0),
                output_tokens: receipt.receipt.output_tokens.unwrap_or(0),
            },
        };
        validate_answers(request, &decision_response)?;
        Ok(decision_response)
    }
}

fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(retry_after) = retry_after {
        return retry_after;
    }
    RETRY_BASE_DELAY
        .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
        .min(Duration::from_secs(30))
}

async fn read_bounded(response: reqwest::Response) -> Result<String, DecisionError> {
    let mut collected: Vec<u8> = Vec::with_capacity(4096);
    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|err| DecisionError::Transport(err.to_string()))?
    {
        if collected.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(DecisionError::Transport(
                "response body exceeds the 1 MiB decision cap".to_string(),
            ));
        }
        collected.extend_from_slice(&chunk);
    }
    String::from_utf8(collected)
        .map_err(|err| DecisionError::Transport(format!("response body not utf-8: {err}")))
}
