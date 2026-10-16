//! Kev fully on Apple's GPU through MLX (`mlx` feature), using
//! codesoda/kev-rs's runtime (`kev-core`, pinned at v0.1.0): the Qwen3.5
//! backbone with the LoRA folded in fp32, a fused Gated DeltaNet Metal
//! kernel, every question batched on the state's cache, and the pointer
//! head. This adapter supplies the record ([`super::kev::record_of`]) and
//! shapes the answers ([`super::kev::answer_from_probs`]) exactly as the
//! ONNX adapter does, so the two differ only in the backbone that runs.
//!
//! A model directory holds `base/` (the Qwen3.5 base snapshot), `adapter/`
//! (the Kev LoRA), `head.safetensors`, and `head.meta.json`
//! (`{"meta": {"temperature": …}}`) — what `make setup-decision-models`
//! installs for `kev-0.8b-mlx` / `kev-4b-mlx`. By default the weights are
//! 8-bit (group size 32) and a new state runs in 512-token passes: Kev-4B
//! holds 4.41 GiB instead of 6.65 and peaks at 5.44 instead of 8.61, with
//! the same answers within kev-rs's gates.
//!
//! MLX state stays on one worker thread that owns the runtime; requests
//! queue to it. Questions of very different lengths run as separate
//! batches on the cached state (see [`run`]).

use std::path::PathBuf;
use std::sync::mpsc;

use async_trait::async_trait;
use kev_core::runtime::{Device, LoadOptions, MlxOptions, Quantization, Runtime};

use super::kev::{answer_from_probs, kev_capabilities, record_of, Record};
use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::request::{validate_answers, DecisionRequest, DecisionResponse, Usage};

#[derive(Debug, Clone)]
pub struct KevMlxConfig {
    pub model_dir: PathBuf,
    /// Model id echoed on responses; thresholds key on it.
    pub model: String,
    /// Override the checkpoint temperature (unset = `head.meta.json`).
    pub temperature: Option<f64>,
    pub max_state: usize,
    pub max_branch: usize,
    /// 8-bit weights (kev-core `Quantization::Q8`); false = bf16.
    pub quantize: bool,
    /// Tokens per pass for a new state; None = one pass.
    pub state_chunk: Option<usize>,
    pub capabilities: ModelCapabilities,
}

impl KevMlxConfig {
    pub fn new(model_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            model_dir: model_dir.into(),
            model: model.into(),
            temperature: None,
            max_state: kev_core::encode::SERVE_MAX_STATE,
            max_branch: kev_core::encode::SERVE_MAX_BRANCH,
            quantize: true,
            state_chunk: Some(512),
            capabilities: kev_capabilities(kev_core::encode::SERVE_MAX_STATE),
        }
    }
}

type Reply = tokio::sync::oneshot::Sender<Result<(Vec<Vec<f64>>, u64), DecisionError>>;

struct Job {
    record: kev_core::Record,
    reply: Reply,
    _lease: Option<std::sync::Arc<crate::dispatch::Reservation>>,
}

pub struct KevMlxModel {
    config: KevMlxConfig,
    jobs: tokio::sync::mpsc::Sender<Job>,
}

fn transport(what: &str, error: impl std::fmt::Display) -> DecisionError {
    DecisionError::Transport(format!("kev-mlx: {what}: {error}"))
}

impl KevMlxModel {
    /// Start the worker and load the model on it. Blocking and heavy (the
    /// LoRA merge and the Metal kernels' first compile).
    pub fn load(config: KevMlxConfig) -> Result<Self, DecisionError> {
        let (jobs, mut queue) = tokio::sync::mpsc::channel::<Job>(1);
        let (ready, loaded) = mpsc::channel::<Result<(), DecisionError>>();
        let options = LoadOptions {
            model_dir: config.model_dir.clone(),
            device: Device::Metal,
            temperature: config.temperature.map(|t| t as f32),
            mlx: MlxOptions {
                quantize: config.quantize.then_some(Quantization::Q8),
                state_chunk: config.state_chunk,
            },
        };
        let (max_state, max_branch) = (config.max_state, config.max_branch);
        std::thread::Builder::new()
            .name("kev-mlx".to_string())
            .spawn(move || {
                if let Err(error) = super::mlx_common::limit_mlx_cache() {
                    let _ = ready.send(Err(transport("cache limit", error)));
                    return;
                }
                let mut runtime = match Runtime::load(&options) {
                    Ok(runtime) => {
                        let _ = ready.send(Ok(()));
                        runtime
                    },
                    Err(error) => {
                        let _ = ready.send(Err(transport("load", error)));
                        return;
                    },
                };
                while let Some(job) = queue.blocking_recv() {
                    // A timed-out classification may still be queued behind an
                    // in-flight Metal kernel. Do not execute abandoned work.
                    if job.reply.is_closed() {
                        continue;
                    }
                    let result = run(&mut runtime, &job.record, max_state, max_branch);
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| transport("worker", e))?;
        loaded.recv().map_err(|e| transport("worker", e))??;
        Ok(Self { config, jobs })
    }
}

impl KevMlxModel {
    /// Calibrated probabilities per question for a rendered record, and
    /// the encoded token count — the backbone-and-head scope the kev.js
    /// fixtures check.
    pub async fn probs_for_record(
        &self,
        record: Record,
    ) -> Result<(Vec<Vec<f64>>, u64), DecisionError> {
        let record = kev_core::Record {
            state: record.state,
            questions: record
                .questions
                .into_iter()
                .map(|q| kev_core::api::RecordQuestion {
                    instr: q.instr,
                    options: q.options,
                })
                .collect(),
        };
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job {
                record,
                reply,
                _lease: crate::admission::physical_lease(),
            })
            .await
            .map_err(|e| transport("worker", e))?;
        answer.await.map_err(|e| transport("worker", e))?
    }
}

/// A batch pads every question to its longest, so questions of very
/// different lengths run as separate batches: a new group starts when a
/// question is more than this many times the group's shortest.
const GROUP_SPREAD: f64 = 2.0;

/// Probabilities per question, in record order, and the encoded token
/// count. kev-core batches a request's questions on the state's cache and
/// pads them to the longest; the step judges' element question carries the
/// candidate list and dwarfs the rest, so questions are grouped by length
/// and each group runs as its own batch. Every group after the first reuses
/// the cached state (kev-core keys it by the state's tokens), and each
/// question sees only the state and itself, so grouping changes no answer.
fn run(
    runtime: &mut Runtime,
    record: &kev_core::Record,
    max_state: usize,
    max_branch: usize,
) -> Result<(Vec<Vec<f64>>, u64), DecisionError> {
    let encode = |runtime: &Runtime, record: &kev_core::Record| {
        kev_core::encode::encode(&runtime.tokenizer, record, max_state, max_branch, false)
            .map_err(|e| transport("encode", e))
    };
    let full = encode(runtime, record)?;
    let tokens = full.ids.len() as u64;
    let groups = length_groups(&branch_lengths(&full));
    if groups.len() <= 1 {
        let (probs, _) = runtime
            .probs_encoded(&full)
            .map_err(|e| transport("inference", e))?;
        return Ok((probs, tokens));
    }
    let mut probs = vec![Vec::new(); record.questions.len()];
    for group in groups {
        let sub = kev_core::Record {
            state: record.state.clone(),
            questions: group.iter().map(|&i| record.questions[i].clone()).collect(),
        };
        let (group_probs, _) = runtime
            .probs_encoded(&encode(runtime, &sub)?)
            .map_err(|e| transport("inference", e))?;
        for (&i, p) in group.iter().zip(group_probs) {
            probs[i] = p;
        }
    }
    Ok((probs, tokens))
}

/// Each question's branch length in tokens, from an encoding's
/// `<decide>` positions (a branch runs from the previous one, or the end of
/// the state, through its own).
fn branch_lengths(encoding: &kev_core::encode::Encoding) -> Vec<usize> {
    let mut start = encoding.seg.iter().take_while(|&&s| s == 0).count();
    encoding
        .decide_idx
        .iter()
        .map(|&decide| {
            let len = decide + 1 - start;
            start = decide + 1;
            len
        })
        .collect()
}

/// Question indices grouped so no group spans more than [`GROUP_SPREAD`]
/// in branch length; groups in ascending length.
fn length_groups(lengths: &[usize]) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by_key(|&i| lengths[i]);
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in order {
        match groups.last_mut() {
            Some(group) if lengths[i] as f64 <= lengths[group[0]] as f64 * GROUP_SPREAD => {
                group.push(i)
            },
            _ => groups.push(vec![i]),
        }
    }
    groups
}

#[async_trait]
impl StructuredDecisionModel for KevMlxModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("kev-mlx", &self.config.model)
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.config.capabilities.clone()
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let (record, keys) = record_of(&request);
        let (probs, tokens) = self.probs_for_record(record).await?;
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

#[cfg(test)]
mod tests {
    use super::length_groups;

    #[test]
    fn questions_group_by_length() {
        // Six short questions and one long element question: two batches.
        assert_eq!(
            length_groups(&[40, 900, 42, 55, 38, 60, 44]),
            vec![vec![4, 0, 2, 6, 3, 5], vec![1]]
        );
        assert_eq!(length_groups(&[10, 12, 14]), vec![vec![0, 1, 2]]);
        assert!(length_groups(&[]).is_empty());
    }
}
