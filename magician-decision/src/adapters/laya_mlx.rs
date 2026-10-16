//! laya on Apple's GPU through MLX (`mlx` feature): the mmBERT encoder
//! (ModernBERT: alternating full and 128-token sliding-window attention,
//! GeGLU MLP, bias-free LayerNorms), the two-layer decision head, and the
//! marker scorer, ported from laya-mlx's `model.py` (itself a port of the
//! original Laya PyTorch model). The action head is not computed: nothing
//! here reads it.
//!
//! Rows, calibration, and answers are [`super::laya`]'s, shared with
//! `laya-onnx`, so the two differ only in the network that runs. Unlike
//! the ONNX export, padding is masked out of every key, so a row's answer
//! does not depend on the batch it rides in.
//!
//! A model directory holds `model.safetensors` (the original PyTorch
//! weights), `encoder/config.json`, `rl_agent_config.json`,
//! `tokenizer.json`, and `tokenizer_config.json` — what `make
//! setup-decision-models MODELS="laya-multilingual-mlx"` installs. MLX
//! state stays on one worker thread; requests queue to it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use async_trait::async_trait;
use mlx_rs::transforms::compile::compile;
use mlx_rs::{fast, nn, ops, transforms::eval, Array, Dtype};
use serde::Deserialize;

use super::laya::{answer_from_logits, build_rows, collate, Batch, Row};
use super::laya_assets::{asset_error, laya_capabilities, load_assets, LayaAssets};
use crate::error::DecisionError;
use crate::model::{ModelCapabilities, ModelIdentity, StructuredDecisionModel};
use crate::request::{validate_answers, DecisionRequest, DecisionResponse, Usage};

type AnyResult<T> = Result<T, mlx_rs::error::Exception>;

#[derive(Debug, Clone)]
pub struct LayaMlxConfig {
    pub model_dir: PathBuf,
    /// Model id echoed on responses; thresholds key on it.
    pub model: String,
    /// Compute dtype: the stored weights are float16 (laya-mlx's default
    /// too); float32 is the reference precision.
    pub dtype: LayaDtype,
    /// Compile each layer (MLX graph compilation), padding batch lengths to
    /// a multiple of 64 so few shapes are traced. Off by default: measured
    /// 2-7% faster on small inputs and 6% slower on a 4,000-token batch
    /// (the padding), as compiling buys laya-mlx itself ~4%. The remaining
    /// gap to Python laya-mlx (~1.25x on large inputs, more on tiny ones)
    /// is GPU time, not graph building; its cause is not yet identified
    /// (the build is not JIT, and a Metal 4 build did not change it).
    pub compile: bool,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayaDtype {
    Float16,
    Float32,
}

impl LayaMlxConfig {
    pub fn new(model_dir: impl Into<PathBuf>, model: impl Into<String>) -> Self {
        Self {
            model_dir: model_dir.into(),
            model: model.into(),
            dtype: LayaDtype::Float16,
            compile: false,
            capabilities: laya_capabilities(None),
        }
    }
}

// ------------------------------------------------------------------ config --

#[derive(Debug, Deserialize)]
struct RopeKind {
    rope_theta: f32,
}

#[derive(Debug, Deserialize)]
struct EncoderConfig {
    hidden_size: i32,
    num_hidden_layers: usize,
    num_attention_heads: i32,
    #[serde(default = "default_norm_eps")]
    norm_eps: f32,
    #[serde(default = "default_local_attention")]
    local_attention: i32,
    #[serde(default = "default_every")]
    global_attn_every_n_layers: usize,
    #[serde(default = "default_global_theta")]
    global_rope_theta: f32,
    #[serde(default = "default_local_theta")]
    local_rope_theta: f32,
    #[serde(default)]
    layer_types: Option<Vec<String>>,
    #[serde(default)]
    rope_parameters: Option<HashMap<String, RopeKind>>,
}

fn default_norm_eps() -> f32 {
    1e-5
}
fn default_local_attention() -> i32 {
    128
}
fn default_every() -> usize {
    3
}
fn default_global_theta() -> f32 {
    160_000.0
}
fn default_local_theta() -> f32 {
    10_000.0
}

impl EncoderConfig {
    fn is_global(&self, layer: usize) -> bool {
        match &self.layer_types {
            Some(types) => types.get(layer).is_some_and(|t| t == "full_attention"),
            None => layer.is_multiple_of(self.global_attn_every_n_layers),
        }
    }

    fn rope_base(&self, global: bool) -> f32 {
        let kind = if global {
            "full_attention"
        } else {
            "sliding_attention"
        };
        self.rope_parameters
            .as_ref()
            .and_then(|p| p.get(kind))
            .map(|k| k.rope_theta)
            .unwrap_or(if global {
                self.global_rope_theta
            } else {
                self.local_rope_theta
            })
    }
}

// ----------------------------------------------------------------- weights --

struct Linear {
    weight: Array,
    bias: Option<Array>,
}

impl Linear {
    /// `x·Wᵀ (+ b)`, the bias fused through `addmm` as MLX's `nn.Linear`
    /// does, so float16 rounds as laya-mlx does.
    fn apply(&self, x: &Array) -> AnyResult<Array> {
        let wt = ops::swap_axes(&self.weight, -1, -2)?;
        match &self.bias {
            Some(bias) => ops::addmm(bias, x, &wt, None, None),
            None => ops::matmul(x, &wt),
        }
    }
}

struct Norm {
    weight: Array,
    bias: Option<Array>,
    eps: f32,
}

impl Norm {
    fn apply(&self, x: &Array) -> AnyResult<Array> {
        fast::layer_norm(x, &self.weight, self.bias.as_ref(), self.eps)
    }
}

struct EncoderLayer {
    /// None on layer 0 (the embeddings are already normalized).
    attn_norm: Option<Norm>,
    wqkv: Linear,
    wo: Linear,
    mlp_norm: Norm,
    wi: Linear,
    mlp_wo: Linear,
    global: bool,
    rope_base: f32,
}

struct HeadLayer {
    norm1: Norm,
    in_proj: Linear,
    out_proj: Linear,
    norm2: Norm,
    linear1: Linear,
    linear2: Linear,
}

struct Network {
    embed: Array,
    embed_norm: Norm,
    layers: Vec<EncoderLayer>,
    final_norm: Norm,
    type_emb: Array,
    head: Vec<HeadLayer>,
    scorer_norm: Norm,
    scorer_1: Linear,
    scorer_2: Linear,
    heads: i32,
    decision_heads: i32,
    local_window: i32,
    eps: f32,
    compile: bool,
    compiled: RefCell<HashMap<LayerKey, CompiledLayer>>,
}

struct Weights {
    map: HashMap<String, Array>,
    dtype: Dtype,
}

impl Weights {
    fn take(&mut self, name: &str) -> Result<Array, String> {
        let array = self
            .map
            .remove(name)
            .ok_or_else(|| format!("missing weight {name}"))?;
        array
            .as_dtype(self.dtype)
            .map_err(|e| format!("{name}: {e}"))
    }

    fn linear(&mut self, name: &str, bias: bool) -> Result<Linear, String> {
        Ok(Linear {
            weight: self.take(&format!("{name}.weight"))?,
            bias: if bias {
                Some(self.take(&format!("{name}.bias"))?)
            } else {
                None
            },
        })
    }

    /// PyTorch `MultiheadAttention` packs its input projection as
    /// `in_proj_weight` / `in_proj_bias`.
    fn packed_in_proj(&mut self, name: &str) -> Result<Linear, String> {
        Ok(Linear {
            weight: self.take(&format!("{name}.in_proj_weight"))?,
            bias: Some(self.take(&format!("{name}.in_proj_bias"))?),
        })
    }

    fn norm(&mut self, name: &str, bias: bool, eps: f32) -> Result<Norm, String> {
        Ok(Norm {
            weight: self.take(&format!("{name}.weight"))?,
            bias: if bias {
                Some(self.take(&format!("{name}.bias"))?)
            } else {
                None
            },
            eps,
        })
    }
}

impl Network {
    fn load(
        dir: &Path,
        head_layers: usize,
        dtype: LayaDtype,
        compile: bool,
    ) -> Result<Self, String> {
        let config: EncoderConfig = serde_json::from_str(
            &std::fs::read_to_string(dir.join("encoder/config.json"))
                .map_err(|e| format!("encoder/config.json: {e}"))?,
        )
        .map_err(|e| format!("encoder/config.json: {e}"))?;
        if config.hidden_size % config.num_attention_heads != 0 {
            return Err("hidden size is not a multiple of the head count".into());
        }
        let dtype = match dtype {
            LayaDtype::Float16 => Dtype::Float16,
            LayaDtype::Float32 => Dtype::Float32,
        };
        let map = Array::load_safetensors(dir.join("model.safetensors"))
            .map_err(|e| format!("model.safetensors: {e}"))?;
        let mut w = Weights { map, dtype };
        let eps = config.norm_eps;
        let mut layers = Vec::with_capacity(config.num_hidden_layers);
        for i in 0..config.num_hidden_layers {
            let p = format!("encoder.layers.{i}");
            let global = config.is_global(i);
            layers.push(EncoderLayer {
                attn_norm: if i == 0 {
                    None
                } else {
                    Some(w.norm(&format!("{p}.attn_norm"), false, eps)?)
                },
                wqkv: w.linear(&format!("{p}.attn.Wqkv"), false)?,
                wo: w.linear(&format!("{p}.attn.Wo"), false)?,
                mlp_norm: w.norm(&format!("{p}.mlp_norm"), false, eps)?,
                wi: w.linear(&format!("{p}.mlp.Wi"), false)?,
                mlp_wo: w.linear(&format!("{p}.mlp.Wo"), false)?,
                global,
                rope_base: config.rope_base(global),
            });
        }
        let mut head = Vec::with_capacity(head_layers);
        for i in 0..head_layers {
            let p = format!("head.layers.{i}");
            head.push(HeadLayer {
                norm1: w.norm(&format!("{p}.norm1"), true, 1e-5)?,
                in_proj: w.packed_in_proj(&format!("{p}.self_attn"))?,
                out_proj: w.linear(&format!("{p}.self_attn.out_proj"), true)?,
                norm2: w.norm(&format!("{p}.norm2"), true, 1e-5)?,
                linear1: w.linear(&format!("{p}.linear1"), true)?,
                linear2: w.linear(&format!("{p}.linear2"), true)?,
            });
        }
        let dims = config.hidden_size;
        let network = Self {
            embed: w.take("encoder.embeddings.tok_embeddings.weight")?,
            embed_norm: w.norm("encoder.embeddings.norm", false, eps)?,
            layers,
            final_norm: w.norm("encoder.final_norm", false, eps)?,
            type_emb: w.take("type_emb.weight")?,
            head,
            scorer_norm: w.norm("scorer.0", true, 1e-5)?,
            scorer_1: w.linear("scorer.1", true)?,
            scorer_2: w.linear("scorer.3", true)?,
            heads: config.num_attention_heads,
            decision_heads: (dims / 64).max(1),
            local_window: config.local_attention,
            eps,
            compile,
            compiled: RefCell::new(HashMap::new()),
        };
        eval(network.arrays()).map_err(|e| format!("weights: {e}"))?;
        Ok(network)
    }

    /// Every weight array, for one eager evaluation at load.
    fn arrays(&self) -> Vec<&Array> {
        let mut linears: Vec<&Linear> = vec![&self.scorer_1, &self.scorer_2];
        let mut norms: Vec<&Norm> = vec![&self.embed_norm, &self.final_norm, &self.scorer_norm];
        for layer in &self.layers {
            linears.extend([&layer.wqkv, &layer.wo, &layer.wi, &layer.mlp_wo]);
            norms.push(&layer.mlp_norm);
            norms.extend(layer.attn_norm.as_ref());
        }
        for layer in &self.head {
            linears.extend([
                &layer.in_proj,
                &layer.out_proj,
                &layer.linear1,
                &layer.linear2,
            ]);
            norms.extend([&layer.norm1, &layer.norm2]);
        }
        let mut all = vec![&self.embed, &self.type_emb];
        for linear in linears {
            all.push(&linear.weight);
            all.extend(linear.bias.as_ref());
        }
        for norm in norms {
            all.push(&norm.weight);
            all.extend(norm.bias.as_ref());
        }
        all
    }

    /// Masked, per-row marker logits [n, k] (float32; -1e4 where a row has
    /// no marker), as laya-mlx's `DecisionModel.__call__` computes them.
    fn logits(&self, batch: &Batch) -> AnyResult<Array> {
        let (n, l, k) = (
            batch.rows as i32,
            batch.seq_len as i32,
            batch.markers as i32,
        );
        let ids: Vec<i32> = batch.input_ids.iter().map(|&v| v as i32).collect();
        let ids = Array::from_slice(&ids, &[n, l]);
        let valid: Vec<bool> = batch.attention_mask.iter().map(|&m| m == 1).collect();
        let valid = Array::from_slice(&valid, &[n, l]);

        // Key masks: every query sees valid keys only; sliding layers also
        // limit the distance to local_attention / 2 (inclusive). A padded
        // query sees all valid keys (never read, but never an empty row).
        let full = valid.reshape(&[n, 1, 1, l])?;
        let positions = Array::arange::<_, i32>(None, l, None)?;
        let distance = positions
            .reshape(&[l, 1])?
            .subtract(&positions.reshape(&[1, l])?)?
            .abs()?;
        let near = distance
            .le(Array::from_int(self.local_window / 2))?
            .reshape(&[1, 1, l, l])?;
        let padded_query = valid.logical_not()?.reshape(&[n, 1, l, 1])?;
        let local = near.logical_or(&padded_query)?.logical_and(&full)?;

        let mut h = self.embed_norm.apply(&self.embed.take_axis(&ids, 0)?)?;
        // Layer by layer for large batches only: as one lazy graph their
        // intermediates stay live (peak footprint 3.5 GB over varied steps,
        // 1.9 GB evaluated per layer, same latency), while on a small input
        // 22 synchronizations cost ~3.5 ms of a ~10 ms call.
        let eval_each = (n * l) as usize >= EVAL_EACH_LAYER_TOKENS;
        let mut compiled = self.compiled.borrow_mut();
        for layer in &self.layers {
            let mask = if layer.global { &full } else { &local };
            let mut args = vec![h, mask.clone()];
            args.extend(layer.attn_norm.as_ref().map(|n| n.weight.clone()));
            args.extend([
                layer.wqkv.weight.clone(),
                layer.wo.weight.clone(),
                layer.mlp_norm.weight.clone(),
                layer.wi.weight.clone(),
                layer.mlp_wo.weight.clone(),
            ]);
            let (heads, base, eps, has_norm) = (
                self.heads,
                layer.rope_base,
                self.eps,
                layer.attn_norm.is_some(),
            );
            h = if self.compile {
                let key = LayerKey::Encoder(has_norm, base.to_bits());
                let f = compiled.entry(key).or_insert_with(|| {
                    Box::new(compile(
                        move |a: &[Array]| {
                            encoder_layer(a, heads, base, eps, has_norm).map(|h| vec![h])
                        },
                        false,
                    ))
                });
                f(&args)?.remove(0)
            } else {
                encoder_layer(&args, heads, base, eps, has_norm)?
            };
            if eval_each {
                eval([&h])?;
            }
        }
        h = self.final_norm.apply(&h)?;

        let qtype: Vec<i32> = batch.qtype.iter().map(|&q| q as i32).collect();
        let qtype = Array::from_slice(&qtype, &[n]);
        h = h.add(&self.type_emb.take_axis(&qtype, 0)?.reshape(&[n, 1, -1])?)?;
        for layer in &self.head {
            let mut args = vec![h, full.clone()];
            for norm in [&layer.norm1, &layer.norm2] {
                args.push(norm.weight.clone());
                args.extend(norm.bias.clone());
            }
            for linear in [
                &layer.in_proj,
                &layer.out_proj,
                &layer.linear1,
                &layer.linear2,
            ] {
                args.push(linear.weight.clone());
                args.extend(linear.bias.clone());
            }
            let heads = self.decision_heads;
            h = if self.compile {
                let f = compiled.entry(LayerKey::Head).or_insert_with(|| {
                    Box::new(compile(
                        move |a: &[Array]| head_layer(a, heads).map(|h| vec![h]),
                        false,
                    ))
                });
                f(&args)?.remove(0)
            } else {
                head_layer(&args, heads)?
            };
        }

        let positions: Vec<i32> = batch.marker_pos.iter().map(|&p| p.max(0) as i32).collect();
        let positions = Array::from_slice(&positions, &[n, k, 1]);
        let markers = h.take_along_axis(&positions, 1)?;
        let hidden = nn::gelu(&self.scorer_1.apply(&self.scorer_norm.apply(&markers)?)?)?;
        let logits = self
            .scorer_2
            .apply(&hidden)?
            .reshape(&[n, k])?
            .as_dtype(Dtype::Float32)?;
        let marker_mask = Array::from_slice(&batch.marker_mask, &[n, k]);
        let floor = Array::from_f32(-1e4);
        ops::select(&marker_mask, &logits, &floor)
    }
}

/// A batch at least this many tokens (rows x length) is evaluated layer by
/// layer (see [`Network::logits`]).
const EVAL_EACH_LAYER_TOKENS: usize = 2048;

/// Compiled layer functions, one per layer kind: MLX traces each input
/// shape once, so batch lengths are padded to [`PAD_TO_MULTIPLE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LayerKey {
    /// (has the attention pre-norm, RoPE base bits)
    Encoder(bool, u32),
    Head,
}

type CompiledLayer = Box<dyn for<'a> FnMut(&'a [Array]) -> AnyResult<Vec<Array>>>;

/// Batch lengths round up to a multiple of this when compiling, bounding
/// the shapes traced (padding is masked from every key, so answers do not
/// change).
const PAD_TO_MULTIPLE: usize = 64;

fn linear(x: &Array, weight: &Array, bias: Option<&Array>) -> AnyResult<Array> {
    let wt = ops::swap_axes(weight, -1, -2)?;
    match bias {
        Some(bias) => ops::addmm(bias, x, &wt, None, None),
        None => ops::matmul(x, &wt),
    }
}

/// Multi-head attention over `x` [b, L, dims] with a boolean key mask
/// broadcastable to [b, heads, L, L]; RoPE when `rope_base` is set.
#[allow(clippy::too_many_arguments)]
fn attention(
    x: &Array,
    wqkv: &Array,
    bqkv: Option<&Array>,
    wo: &Array,
    bo: Option<&Array>,
    heads: i32,
    rope_base: Option<f32>,
    mask: &Array,
) -> AnyResult<Array> {
    let shape = x.shape();
    let (b, l) = (shape[0], shape[1]);
    let head_dim = shape[2] / heads;
    let parts = linear(x, wqkv, bqkv)?
        .reshape(&[b, l, 3, heads, head_dim])?
        .split_equal(3, 2)?;
    // Each part as a view: squeeze the split axis rather than reshape,
    // which would copy the strided slice (Python's qkv[:, :, i]).
    let project = |part: &Array| -> AnyResult<Array> {
        let t = part.squeeze_axes(&[2])?.transpose_axes(&[0, 2, 1, 3])?;
        match rope_base {
            Some(base) => fast::rope(&t, head_dim, false, base, 1.0, 0, None),
            None => Ok(t),
        }
    };
    let q = project(&parts[0])?;
    let k = project(&parts[1])?;
    let v = parts[2].squeeze_axes(&[2])?.transpose_axes(&[0, 2, 1, 3])?;
    let scale = (head_dim as f32).powf(-0.5);
    let o = fast::scaled_dot_product_attention(&q, &k, &v, scale, mask, None)?;
    linear(
        &o.transpose_axes(&[0, 2, 1, 3])?.reshape(&[b, l, -1])?,
        wo,
        bo,
    )
}

/// One ModernBERT layer. `args`: h, mask, [attn_norm], Wqkv, Wo, mlp_norm,
/// Wi, mlp Wo.
fn encoder_layer(
    args: &[Array],
    heads: i32,
    rope_base: f32,
    eps: f32,
    has_norm: bool,
) -> AnyResult<Array> {
    let (h, mask) = (&args[0], &args[1]);
    let w = &args[if has_norm { 3 } else { 2 }..];
    let normed = if has_norm {
        fast::layer_norm(h, &args[2], None, eps)?
    } else {
        h.clone()
    };
    let h = h.add(&attention(
        &normed,
        &w[0],
        None,
        &w[1],
        None,
        heads,
        Some(rope_base),
        mask,
    )?)?;
    let wi = linear(&fast::layer_norm(&h, &w[2], None, eps)?, &w[3], None)?.split_equal(2, -1)?;
    h.add(&linear(&nn::gelu(&wi[0])?.multiply(&wi[1])?, &w[4], None)?)
}

/// One decision-head layer (PyTorch `TransformerEncoderLayer`, norm first,
/// ReLU). `args`: h, mask, norm1 w/b, norm2 w/b, in_proj w/b, out_proj w/b,
/// linear1 w/b, linear2 w/b.
fn head_layer(args: &[Array], heads: i32) -> AnyResult<Array> {
    let (h, mask) = (&args[0], &args[1]);
    let a = &args[2..];
    let normed = fast::layer_norm(h, &a[0], &a[1], 1e-5)?;
    let h = h.add(&attention(
        &normed,
        &a[4],
        Some(&a[5]),
        &a[6],
        Some(&a[7]),
        heads,
        None,
        mask,
    )?)?;
    let hidden = nn::relu(&linear(
        &fast::layer_norm(&h, &a[2], &a[3], 1e-5)?,
        &a[8],
        Some(&a[9]),
    )?)?;
    h.add(&linear(&hidden, &a[10], Some(&a[11]))?)
}

// ----------------------------------------------------------------- adapter --

type Reply = tokio::sync::oneshot::Sender<Result<Vec<Vec<f32>>, DecisionError>>;

struct Job {
    rows: Vec<Row>,
    reply: Reply,
    _lease: Option<std::sync::Arc<crate::dispatch::Reservation>>,
}

pub struct LayaMlxModel {
    config: LayaMlxConfig,
    assets: Arc<LayaAssets>,
    jobs: tokio::sync::mpsc::Sender<Job>,
}

fn transport(what: &str, error: impl std::fmt::Display) -> DecisionError {
    DecisionError::Transport(format!("laya-mlx: {what}: {error}"))
}

/// One batch of rows through the network: each row's logits, trimmed to
/// its own markers.
fn run(network: &Network, rows: &[Row], pad: u32) -> Result<Vec<Vec<f32>>, DecisionError> {
    let mut batch = collate(rows, pad);
    if network.compile {
        pad_length(&mut batch, PAD_TO_MULTIPLE, pad);
    }
    let logits = network
        .logits(&batch)
        .map_err(|e| transport("inference", e))?;
    eval([&logits]).map_err(|e| transport("inference", e))?;
    let flat: &[f32] = logits.as_slice();
    let k = batch.markers;
    Ok(rows
        .iter()
        .enumerate()
        .map(|(i, row)| flat[i * k..i * k + row.markers.len()].to_vec())
        .collect())
}

/// Extend every row to the next multiple of `multiple` with masked pads.
fn pad_length(batch: &mut Batch, multiple: usize, pad: u32) {
    let (l, target) = (batch.seq_len, batch.seq_len.div_ceil(multiple) * multiple);
    if target == l {
        return;
    }
    let widen = |values: &[i64], fill: i64| -> Vec<i64> {
        values
            .chunks(l)
            .flat_map(|row| {
                row.iter()
                    .copied()
                    .chain(std::iter::repeat_n(fill, target - l))
            })
            .collect()
    };
    batch.input_ids = widen(&batch.input_ids, pad as i64);
    batch.attention_mask = widen(&batch.attention_mask, 0);
    batch.seq_len = target;
}

impl LayaMlxModel {
    /// Load the assets, then the network on its worker thread. Blocking.
    pub fn load(mut config: LayaMlxConfig) -> Result<Self, DecisionError> {
        let assets = Arc::new(load_assets(&config.model_dir)?);
        config
            .capabilities
            .max_state_tokens
            .get_or_insert(assets.agent.max_len as u64);
        let (jobs, mut queue) = tokio::sync::mpsc::channel::<Job>(1);
        let (ready, loaded) = mpsc::channel::<Result<(), DecisionError>>();
        let (dir, dtype, compile) = (config.model_dir.clone(), config.dtype, config.compile);
        let (head_layers, pad) = (assets.agent.head_layers, assets.special.pad);
        std::thread::Builder::new()
            .name("laya-mlx".to_string())
            .spawn(move || {
                if let Err(error) = super::mlx_common::limit_mlx_cache() {
                    let _ = ready.send(Err(transport("cache limit", error)));
                    return;
                }
                let network = match Network::load(&dir, head_layers, dtype, compile) {
                    Ok(network) => {
                        let _ = ready.send(Ok(()));
                        network
                    },
                    Err(error) => {
                        let _ = ready.send(Err(asset_error("load", &dir, error)));
                        return;
                    },
                };
                // Keep only the network's arrays; the file's action head
                // and staging copies go back to the system.
                let _ = mlx_rs::memory::clear_cache();
                while let Some(job) = queue.blocking_recv() {
                    if job.reply.is_closed() {
                        continue;
                    }
                    let _ = job.reply.send(run(&network, &job.rows, pad));
                }
            })
            .map_err(|e| transport("worker", e))?;
        loaded.recv().map_err(|e| transport("worker", e))??;
        Ok(Self {
            config,
            assets,
            jobs,
        })
    }

    /// A request's rows, one per question, as this model builds them.
    pub fn rows(&self, request: &DecisionRequest) -> Result<Vec<Row>, DecisionError> {
        let assets = &self.assets;
        let encode = |text: &str| -> Vec<u32> {
            assets
                .tokenizer
                .encode(text, false)
                .map(|encoding| encoding.get_ids().to_vec())
                .unwrap_or_default()
        };
        build_rows(&encode, &assets.special, &assets.agent, request)
    }

    /// Logits for prepared rows.
    pub async fn logits_for_rows(&self, rows: Vec<Row>) -> Result<Vec<Vec<f32>>, DecisionError> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Job {
                rows,
                reply,
                _lease: crate::admission::physical_lease(),
            })
            .await
            .map_err(|e| transport("worker", e))?;
        answer.await.map_err(|e| transport("worker", e))?
    }
}

#[async_trait]
impl StructuredDecisionModel for LayaMlxModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("laya-mlx", &self.config.model)
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.config.capabilities.clone()
    }

    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        let rows = self.rows(&request)?;
        let assets = &self.assets;
        let input_tokens = rows.iter().map(|r| r.ids.len() as u64).sum();
        let logits = self.logits_for_rows(rows).await?;
        let answers = request
            .questions
            .iter()
            .zip(logits)
            .map(|(question, row)| {
                (
                    question.id().clone(),
                    answer_from_logits(&assets.agent, question, &row),
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
