//! laya on ONNX Runtime (`onnx` feature): the session half of
//! [`super::laya`]. One batched forward pass per request — a row per
//! question — on the engine's own threads; no Python, no extra process.
//!
//! A model directory holds `model.onnx` (the graph: `input_ids`,
//! `attention_mask`, `marker_pos`, `marker_mask`, `qtype` → `logits`,
//! `act_logits`), `tokenizer.json`, `tokenizer_config.json` (special
//! tokens), and `rl_agent_config.json` (lengths and temperatures).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::kev_onnx::open_session;
use async_trait::async_trait;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::laya::{answer_from_logits, build_rows, collate, LayaAgentConfig, SpecialTokens};
pub use super::laya_assets::laya_capabilities;
use super::laya_assets::{asset_error, load_assets, LayaAssets};
use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::request::{validate_answers, DecisionRequest, DecisionResponse, Usage};

#[derive(Debug, Clone)]
pub struct LayaOnnxConfig {
    /// Directory with the model files (see the module docs).
    pub model_dir: PathBuf,
    /// Graph file inside `model_dir`.
    pub model_file: String,
    /// Model id echoed on responses; thresholds key on it.
    pub model: String,
    /// ONNX Runtime intra-op threads.
    pub threads: usize,
    pub capabilities: ModelCapabilities,
}

impl LayaOnnxConfig {
    pub fn new(model_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            model_dir: model_dir.into(),
            model_file: "model.onnx".to_string(),
            model: model.into(),
            threads: default_threads(),
            capabilities: laya_capabilities(None),
        }
    }
}

/// What laya honestly promises: local, uncalibrated until its temperatures
/// are refit, its checkpoint's `max_len` tokens of state (filled in at load
/// when unset), and Choice degrading past ~64 options (the reference
/// reports a sharp fall past ~77).
struct Loaded {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    special: SpecialTokens,
    agent: LayaAgentConfig,
}

pub struct LayaOnnxModel {
    config: LayaOnnxConfig,
    loaded: Arc<Loaded>,
}

impl LayaOnnxModel {
    /// Load the graph, tokenizer, and configs. Blocking and heavy: call it
    /// once per model at bind time.
    pub fn load(mut config: LayaOnnxConfig) -> Result<Self, DecisionError> {
        let LayaAssets {
            tokenizer,
            special,
            agent,
        } = load_assets(&config.model_dir)?;
        config
            .capabilities
            .max_state_tokens
            .get_or_insert(agent.max_len as u64);
        let graph = config.model_dir.join(&config.model_file);
        let session = open_session(&graph, config.threads.max(1))
            .map_err(|e| asset_error("session", &graph, e))?;
        Ok(Self {
            config,
            loaded: Arc::new(Loaded {
                session: Mutex::new(session),
                tokenizer,
                special,
                agent,
            }),
        })
    }

    pub fn agent_config(&self) -> &LayaAgentConfig {
        &self.loaded.agent
    }
}

fn run(loaded: &Loaded, request: &DecisionRequest) -> Result<(Vec<Vec<f32>>, u64), DecisionError> {
    let encode = |text: &str| -> Vec<u32> {
        loaded
            .tokenizer
            .encode(text, false)
            .map(|encoding| encoding.get_ids().to_vec())
            .unwrap_or_default()
    };
    let rows = build_rows(&encode, &loaded.special, &loaded.agent, request)?;
    let mut session = loaded
        .session
        .lock()
        .map_err(|_| DecisionError::Transport("laya: session poisoned".to_string()))?;
    // The reference's padded batch, run a row at a time. Rows of a batch
    // never interact, so each row's logits are the batched ones exactly —
    // but padding is not neutral in this export (a row run at its own
    // length answers differently), so every row keeps the batch's padded
    // length. Peak memory is then one row's attention, not every row's at
    // once: batched, 7 rows of ~1,000 tokens peaked at 5.6 GB.
    let batch = collate(&rows, loaded.special.pad);
    let (l, k) = (batch.seq_len, batch.markers);
    let tensor_error = |e: ort::Error| DecisionError::Transport(format!("laya: tensor: {e}"));
    let mut per_row = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([1, l], batch.input_ids[i * l..(i + 1) * l].to_vec())).map_err(tensor_error)?,
            "attention_mask" => Tensor::from_array(([1, l], batch.attention_mask[i * l..(i + 1) * l].to_vec())).map_err(tensor_error)?,
            "marker_pos" => Tensor::from_array(([1, k], batch.marker_pos[i * k..(i + 1) * k].to_vec())).map_err(tensor_error)?,
            "marker_mask" => Tensor::from_array(([1, k], batch.marker_mask[i * k..(i + 1) * k].to_vec())).map_err(tensor_error)?,
            "qtype" => Tensor::from_array(([1], vec![batch.qtype[i]])).map_err(tensor_error)?,
        ];
        let outputs = session
            .run(inputs)
            .map_err(|e| DecisionError::Transport(format!("laya: inference: {e}")))?;
        let (_, logits) = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| DecisionError::InvalidResponse(format!("laya: logits: {e}")))?;
        per_row.push(logits[..row.markers.len()].to_vec());
    }
    let tokens = batch.attention_mask.iter().filter(|&&m| m == 1).count() as u64;
    Ok((per_row, tokens))
}

#[async_trait]
impl StructuredDecisionModel for LayaOnnxModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("laya-onnx", &self.config.model)
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.config.capabilities.clone()
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let loaded = Arc::clone(&self.loaded);
        let evaluated = request.clone();
        let lease = crate::admission::physical_lease();
        let (logits, input_tokens) = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            run(&loaded, &evaluated)
        })
        .await
        .map_err(|e| DecisionError::Transport(format!("laya: worker: {e}")))??;
        let answers = request
            .questions
            .iter()
            .zip(logits)
            .map(|(question, row)| {
                (
                    question.id().clone(),
                    answer_from_logits(&self.loaded.agent, question, &row),
                )
            })
            .collect();
        let response = DecisionResponse {
            model: self.identity(),
            pack_id: request.pack_id.clone(),
            pack_version: request.pack_version.clone(),
            answers,
            usage: Usage {
                input_tokens,
                output_tokens: 0,
            },
        };
        validate_answers(&request, &response)?;
        Ok(response)
    }
}

/// ONNX Runtime intra-op threads when the model entry names none (see
/// [`super::kev_onnx::default_threads`]).
pub fn default_threads() -> usize {
    super::kev_onnx::default_threads()
}
