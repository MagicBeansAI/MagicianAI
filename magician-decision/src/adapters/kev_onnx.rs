//! Kev on ONNX Runtime (`onnx` feature): the session half of
//! [`super::kev`] (kev.js `model.ts`). The state runs once; each question
//! runs as a continuation of the state's caches — the full-attention
//! layers' KV plus the Gated DeltaNet conv and recurrent states — and the
//! pointer head reads that branch's `<decide>` and `</opt>` hidden states.
//! Branches never see each other.
//!
//! A model directory holds a kev.js bundle variant flattened: `manifest.json`,
//! `model.onnx` with its external data shards beside it, `head.safetensors`,
//! and `tokenizer.json`. The graph needs ONNX Runtime >= 1.30 (contrib ops
//! `LinearAttention*`, `CausalConvWithState`, `GatedRMSNorm`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{OutputSelector, RunOptions, Session, SessionInputValue};
use ort::value::{DynValue, Tensor};
use serde::Deserialize;
use tokenizers::Tokenizer;

use super::kev::{
    answer_from_probs, encode, record_of, temperature_for, Encoding, KevSpecial, PointerHead,
    Record,
};
use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::request::{validate_answers, DecisionRequest, DecisionResponse, Usage};

#[derive(Debug, Clone)]
pub struct KevOnnxConfig {
    pub model_dir: PathBuf,
    /// Model id echoed on responses; thresholds key on it.
    pub model: String,
    /// Manifest variant whose I/O the graph has (`q8f32` by default).
    pub variant: String,
    pub threads: usize,
    /// Override the checkpoint temperature (unset = manifest, else the
    /// run's fitted value, else 1).
    pub temperature: Option<f64>,
    /// States whose caches are kept for reuse (LRU), as kev.serve.
    pub state_cache: usize,
    pub max_state: usize,
    pub max_branch: usize,
    pub capabilities: ModelCapabilities,
}

impl KevOnnxConfig {
    pub fn new(model_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            model_dir: model_dir.into(),
            model: model.into(),
            variant: "q8f32".to_string(),
            threads: default_threads(),
            temperature: None,
            state_cache: 4,
            max_state: 8192,
            max_branch: 8192,
            capabilities: kev_capabilities(8192),
        }
    }
}

pub use super::kev::kev_capabilities;

#[derive(Debug, Clone, Deserialize)]
struct IoInfo {
    name: String,
    #[serde(default)]
    empty: Option<Vec<i64>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Variant {
    inputs: Vec<IoInfo>,
    outputs: Vec<IoInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    run: String,
    hidden_size: usize,
    special: KevSpecial,
    #[serde(default)]
    temperature: Option<f64>,
    variants: std::collections::BTreeMap<String, Variant>,
}

struct CachedState {
    key: Vec<u32>,
    len: usize,
    /// Past inputs by name, from the state run's `present.*` outputs.
    past: Vec<(String, DynValue)>,
}

struct Inner {
    session: Session,
    cache: VecDeque<CachedState>,
}

struct Loaded {
    inner: Mutex<Inner>,
    tokenizer: Tokenizer,
    head: PointerHead,
    manifest: Manifest,
    variant: Variant,
    temperature: f64,
}

pub struct KevOnnxModel {
    config: KevOnnxConfig,
    loaded: Arc<Loaded>,
}

fn load_error(what: &str, path: &Path, error: impl std::fmt::Display) -> DecisionError {
    DecisionError::Transport(format!("kev: {what} {}: {error}", path.display()))
}

fn ort_error(what: &'static str) -> impl Fn(ort::Error) -> DecisionError {
    move |e| DecisionError::Transport(format!("kev: {what}: {e}"))
}

/// A CPU session for a local decision model (laya and Kev share it).
///
/// The CPU arena and memory patterns are off: every request has its own
/// shapes (state length, rows, options), so an arena only grows to the
/// largest request seen and keeps it — laya's resident size reached 3 GB
/// over 40 steps with it on. Freed per run, memory follows the request.
pub(crate) fn open_session(graph: &Path, threads: usize) -> Result<Session, String> {
    let builder = Session::builder().map_err(|e| e.to_string())?;
    let builder = builder
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| e.to_string())?;
    let builder = builder
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|e| e.to_string())?;
    let builder = builder
        .with_memory_pattern(false)
        .map_err(|e| e.to_string())?;
    let mut builder = builder
        .with_intra_threads(threads)
        .map_err(|e| e.to_string())?;
    builder.commit_from_file(graph).map_err(|e| e.to_string())
}

impl KevOnnxModel {
    /// Load the manifest, graph, head, and tokenizer. Blocking and heavy.
    pub fn load(config: KevOnnxConfig) -> Result<Self, DecisionError> {
        let dir = &config.model_dir;
        let manifest_path = dir.join("manifest.json");
        let manifest: Manifest = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .map_err(|e| load_error("read", &manifest_path, e))?,
        )
        .map_err(|e| load_error("parse", &manifest_path, e))?;
        let variant = manifest
            .variants
            .get(&config.variant)
            .cloned()
            .ok_or_else(|| load_error("no such variant in", &manifest_path, &config.variant))?;
        let head_path = dir.join("head.safetensors");
        let head = PointerHead::from_safetensors(
            &std::fs::read(&head_path).map_err(|e| load_error("read", &head_path, e))?,
        )?;
        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| load_error("tokenizer", &tokenizer_path, e))?;
        let graph = dir.join("model.onnx");
        let session = open_session(&graph, config.threads.max(1))
            .map_err(|e| load_error("session", &graph, e))?;
        let temperature = config
            .temperature
            .unwrap_or_else(|| temperature_for(&manifest.run, manifest.temperature));
        Ok(Self {
            config,
            loaded: Arc::new(Loaded {
                inner: Mutex::new(Inner {
                    session,
                    cache: VecDeque::new(),
                }),
                tokenizer,
                head,
                manifest,
                variant,
                temperature,
            }),
        })
    }

    /// The serving temperature in use.
    pub fn temperature(&self) -> f64 {
        self.loaded.temperature
    }

    /// Encode a rendered record with this model's tokenizer and delimiters.
    pub fn encode(&self, record: &Record, strict: bool) -> Result<Encoding, DecisionError> {
        let tokenizer = &self.loaded.tokenizer;
        let tokenize = |text: &str| -> Vec<u32> {
            tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().to_vec())
                .unwrap_or_default()
        };
        encode(
            &tokenize,
            record,
            &self.loaded.manifest.special,
            self.config.max_state,
            self.config.max_branch,
            strict,
        )
    }

    /// Per-question probabilities for an encoding at `temperature` (kev.js
    /// `probsEncoded`): the parity surface.
    pub fn probs(
        &self,
        encoding: &Encoding,
        temperature: f64,
    ) -> Result<Vec<Vec<f64>>, DecisionError> {
        run(&self.loaded, encoding, temperature, self.config.state_cache)
    }
}

fn int64_tensor(data: Vec<i64>, shape: Vec<usize>) -> Result<DynValue, DecisionError> {
    Tensor::from_array((shape, data))
        .map(|t| t.into_dyn())
        .map_err(ort_error("tensor"))
}

fn empty_past(variant: &Variant) -> Result<Vec<(String, DynValue)>, DecisionError> {
    variant
        .inputs
        .iter()
        .filter_map(|input| {
            input
                .empty
                .as_ref()
                .map(|shape| (input.name.clone(), shape))
        })
        .map(|(name, shape)| {
            let shape: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
            let n = shape.iter().product();
            Tensor::from_array((shape, vec![0f32; n]))
                .map(|t| (name, t.into_dyn()))
                .map_err(ort_error("empty cache"))
        })
        .collect()
}

/// The inputs every run takes: ids, an all-ones mask over past + new
/// tokens, and mRoPE positions (the text position for all three sections).
fn base_inputs(
    ids: &[u32],
    pos: &[u32],
    past_len: usize,
) -> Result<Vec<(String, DynValue)>, DecisionError> {
    let s = ids.len();
    let positions: Vec<i64> = pos.iter().map(|&p| p as i64).collect();
    Ok(vec![
        (
            "input_ids".to_string(),
            int64_tensor(ids.iter().map(|&i| i as i64).collect(), vec![1, s])?,
        ),
        (
            "attention_mask".to_string(),
            int64_tensor(vec![1; past_len + s], vec![1, past_len + s])?,
        ),
        (
            "position_ids".to_string(),
            int64_tensor(
                [positions.clone(), positions.clone(), positions].concat(),
                vec![3, 1, s],
            )?,
        ),
    ])
}

fn past_name(present: &str) -> String {
    if present.ends_with(".key") || present.ends_with(".value") {
        present.replacen("present.", "past_key_values.", 1)
    } else {
        present.replacen("present.", "past.", 1)
    }
}

fn run_state(
    inner: &mut Inner,
    variant: &Variant,
    ids: &[u32],
) -> Result<CachedState, DecisionError> {
    let pos: Vec<u32> = (0..ids.len() as u32).collect();
    let mut inputs = base_inputs(ids, &pos, 0)?;
    inputs.extend(empty_past(variant)?);
    let feed: Vec<(String, SessionInputValue)> = inputs
        .into_iter()
        .map(|(name, value)| (name, SessionInputValue::from(value)))
        .collect();
    let mut outputs = inner.session.run(feed).map_err(ort_error("state run"))?;
    let mut past = Vec::new();
    for output in variant
        .outputs
        .iter()
        .filter(|o| o.name.starts_with("present"))
    {
        let value = outputs.remove(output.name.as_str()).ok_or_else(|| {
            DecisionError::InvalidResponse(format!("kev: missing {}", output.name))
        })?;
        past.push((past_name(&output.name), value));
    }
    Ok(CachedState {
        key: ids.to_vec(),
        len: ids.len(),
        past,
    })
}

fn run(
    loaded: &Loaded,
    encoding: &Encoding,
    temperature: f64,
    cache_size: usize,
) -> Result<Vec<Vec<f64>>, DecisionError> {
    let mut inner = loaded
        .inner
        .lock()
        .map_err(|_| DecisionError::Transport("kev: session poisoned".to_string()))?;
    let state = match inner.cache.iter().position(|s| s.key == encoding.state) {
        Some(i) => inner.cache.remove(i).expect("present"),
        None => run_state(&mut inner, &loaded.variant, &encoding.state)?,
    };
    // One pass per question. Batching the branches over the shared state
    // is not possible with this graph: its GroupQueryAttention refuses
    // batch > 1 with a multi-token step over past context; batching without
    // the shared cache would recompute the state once per question.
    let result = branches_sequential(&mut inner, loaded, &state, encoding, temperature);
    if cache_size > 0 {
        inner.cache.push_back(state);
        while inner.cache.len() > cache_size {
            inner.cache.pop_front();
        }
    }
    result
}

/// A branch needs only its hidden states: skip the cache outputs the graph
/// would otherwise hand back.
fn hidden_only() -> Result<RunOptions<ort::session::HasSelectedOutputs>, DecisionError> {
    Ok(RunOptions::new()
        .map_err(ort_error("run options"))?
        .with_outputs(OutputSelector::no_default().with("hidden_states")))
}

/// One pass per question, each continuing the state's caches (kev.js).
fn branches_sequential(
    inner: &mut Inner,
    loaded: &Loaded,
    state: &CachedState,
    encoding: &Encoding,
    temperature: f64,
) -> Result<Vec<Vec<f64>>, DecisionError> {
    let d = loaded.manifest.hidden_size;
    let options = hidden_only()?;
    let mut probs = Vec::with_capacity(encoding.branches.len());
    for branch in &encoding.branches {
        let mut feed: Vec<(String, SessionInputValue)> =
            base_inputs(&branch.ids, &branch.pos, state.len)?
                .into_iter()
                .map(|(name, value)| (name, SessionInputValue::from(value)))
                .collect();
        for (name, value) in &state.past {
            feed.push((name.clone(), SessionInputValue::from(value.view())));
        }
        let outputs = inner
            .session
            .run_with_options(feed, &options)
            .map_err(ort_error("branch run"))?;
        let (_, hidden) = outputs["hidden_states"]
            .try_extract_tensor::<f32>()
            .map_err(ort_error("hidden states"))?;
        let row = |i: usize| &hidden[i * d..(i + 1) * d];
        let opts: Vec<&[f32]> = branch.opts.iter().map(|&o| row(o)).collect();
        probs.push(loaded.head.probs(row(branch.decide), &opts, temperature));
    }
    Ok(probs)
}

#[async_trait]
impl StructuredDecisionModel for KevOnnxModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("kev-onnx", &self.config.model)
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.config.capabilities.clone()
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let (record, keys) = record_of(&request);
        let encoding = self.encode(&record, false)?;
        let loaded = Arc::clone(&self.loaded);
        let (temperature, cache_size) = (self.loaded.temperature, self.config.state_cache);
        let tokens = encoding.tokens as u64;
        let lease = crate::admission::physical_lease();
        let probs = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            run(&loaded, &encoding, temperature, cache_size)
        })
        .await
        .map_err(|e| DecisionError::Transport(format!("kev: worker: {e}")))??;
        let answers = request
            .questions
            .iter()
            .zip(keys.iter().zip(&probs))
            .map(|(question, (keys, p))| {
                (question.id().clone(), answer_from_probs(question, keys, p))
            })
            .collect();
        let response = DecisionResponse {
            model: self.identity(),
            pack_id: request.pack_id.clone(),
            pack_version: request.pack_version.clone(),
            answers,
            usage: Usage {
                input_tokens: tokens,
                output_tokens: 0,
            },
        };
        validate_answers(&request, &response)?;
        Ok(response)
    }
}

/// ONNX Runtime intra-op threads when the model entry names none: the
/// machine's parallelism, capped at 8. Measured on an M4 Pro (10
/// performance cores) with Kev-0.8B on a 7-question step: 2 threads
/// 3.2 s, 4 threads 1.6 s, 8 threads 0.93 s, 10 threads 1.05 s.
pub fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8)
}
