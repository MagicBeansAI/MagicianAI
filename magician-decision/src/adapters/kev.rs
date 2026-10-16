//! Kev — a Qwen3.5 decoder with a LoRA and a pointer head that scores
//! options by their hidden states — ported from kev.js (an exact port of
//! `kev/api.py` + `kev/model.py`; upstream ai-ecoverse/kev.js cc820b5).
//! This file is the model-free half: how a request becomes a state row and
//! one branch per question, the pointer head, the checkpoint temperature,
//! and the answer formulas. The ONNX session and its caches live in
//! `kev_onnx.rs` behind the `onnx` feature.
//!
//! Token layout:
//! `<state> …state…` then, per question, a branch continuing the state's
//! positions: `<q> instructions (<opt> option </opt>)… <decide>`. The state
//! runs once; each branch runs as a continuation of its cache, so branches
//! never see each other.

use crate::adapters::systemone::criteria_text;
use crate::error::DecisionError;
use crate::model::ModelCapabilities;
use crate::primitives::{Criteria, Instruction, OptionId, Question};
use crate::request::{Answer, DecisionRequest};

/// The delimiter tokens (reused Qwen special tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub struct KevSpecial {
    pub state: u32,
    pub q: u32,
    pub opt: u32,
    pub opt_end: u32,
    pub decide: u32,
}

/// One question's rendered instruction and option texts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RecordQuestion {
    pub instr: String,
    pub options: Vec<String>,
}

/// The rendered request the encoder reads.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Record {
    pub state: String,
    pub questions: Vec<RecordQuestion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub ids: Vec<u32>,
    pub pos: Vec<u32>,
    /// Offset of `<decide>` within the branch.
    pub decide: usize,
    /// Offsets of each option's `</opt>` within the branch.
    pub opts: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoding {
    pub state: Vec<u32>,
    pub branches: Vec<Branch>,
    pub state_truncated: bool,
    /// Tokens across the state and every branch (Kev's `usage.input_tokens`).
    pub tokens: usize,
}

/// Python's `repr(float)`: shortest round-trip digits, exponent form below
/// 1e-4 and from 1e16.
pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let sign = if x < 0.0 { "-" } else { "" };
    // `{:e}` is Rust's shortest round-trip form: `d.ddde±x`.
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("scientific form");
    let exp: i32 = exp.parse().expect("exponent");
    let digits = mantissa.replace('.', "");
    if !(-4..16).contains(&exp) {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits
        };
        let esign = if exp < 0 { "-" } else { "+" };
        return format!("{sign}{m}e{esign}{:02}", exp.abs());
    }
    if exp >= 0 {
        let e = exp as usize;
        let int: String = if digits.len() > e {
            digits[..=e].to_string()
        } else {
            format!("{digits:0<width$}", width = e + 1)
        };
        let frac = if digits.len() > e + 1 {
            &digits[e + 1..]
        } else {
            "0"
        };
        return format!("{sign}{int}.{frac}");
    }
    format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
}

/// Python's `str()` for a JSON scalar; integral numbers print as ints.
fn py_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return i.to_string();
            }
            if let Some(u) = n.as_u64() {
                return u.to_string();
            }
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 && f.abs() < 1e21 {
                format!("{f:.0}")
            } else {
                py_float(f)
            }
        },
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// kev's `render`: str | object | array flattened to the text the model
/// sees; objects become `key: value` lines, arrays `- item` lines.
pub fn render(value: &serde_json::Value, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|x| format!("{pad}- {}", render(x, indent + 1).trim_start()))
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, x)| {
                if x.is_object() || x.is_array() {
                    format!("{pad}{k}:\n{}", render(x, indent + 1))
                } else {
                    format!("{pad}{k}: {}", render(x, 0))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        scalar => py_scalar(scalar),
    }
}

fn instruction_text(instructions: &Instruction) -> String {
    match instructions {
        Instruction::Text(text) => text.clone(),
        Instruction::Structured(value) => render(value, 0),
    }
}

/// `optionText`: `name`, or `name: desc` when there is a description.
pub fn option_text(name: &str, desc: Option<&str>) -> String {
    match desc {
        Some(desc) if !desc.is_empty() => format!("{name}: {desc}"),
        _ => name.to_string(),
    }
}

fn criterion(criteria: &Criteria) -> String {
    criteria_text(criteria)
}

/// What Kev promises: local, 255 options, Score supported, `max_state`
/// tokens of state; uncalibrated in this plane's sense until thresholds are
/// fitted per model.
pub fn kev_capabilities(max_state: usize) -> ModelCapabilities {
    ModelCapabilities {
        calibrated: false,
        text_only: true,
        max_state_tokens: Some(max_state as u64),
        max_choice_options: Some(255),
        max_questions: None,
        supports_score: true,
        remote: false,
    }
}

/// The rendered record for a request, and each question's option ids (for
/// choice answers) in option order.
pub fn record_of(request: &DecisionRequest) -> (Record, Vec<Vec<OptionId>>) {
    let mut keys = Vec::with_capacity(request.questions.len());
    let questions = request
        .questions
        .iter()
        .map(|question| match question {
            Question::Choice(q) => {
                keys.push(q.criteria.keys().cloned().collect());
                RecordQuestion {
                    instr: instruction_text(&q.instructions),
                    options: q
                        .criteria
                        .iter()
                        .map(|(id, c)| option_text(id.as_str(), Some(&criterion(c))))
                        .collect(),
                }
            },
            Question::Noul(q) => {
                keys.push(Vec::new());
                let (is_true, is_false) = match &q.criteria {
                    Some(c) => (Some(c.is_true.as_str()), c.is_false.as_deref()),
                    None => (None, None),
                };
                RecordQuestion {
                    instr: instruction_text(&q.instructions),
                    options: vec![option_text("no", is_false), option_text("yes", is_true)],
                }
            },
            Question::Score(q) => {
                keys.push(Vec::new());
                RecordQuestion {
                    instr: instruction_text(&q.instructions),
                    options: q.levels.iter().map(criterion).collect(),
                }
            },
        })
        .collect();
    (
        Record {
            state: render(request.state.as_json(), 0),
            questions,
        },
        keys,
    )
}

/// Caller text can never produce a delimiter token: `<|name|>` becomes
/// `<¦name¦>` before tokenizing.
pub fn scrub_specials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let name_len = after
            .char_indices()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map(|(i, _)| i)
            .unwrap_or(after.len());
        if name_len > 0 && after[name_len..].starts_with("|>") {
            out.push_str("<¦");
            out.push_str(&after[..name_len]);
            out.push_str("¦>");
            rest = &after[name_len + 2..];
        } else {
            out.push_str("<|");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `encode`: the state row and one branch per question. `max_state`
/// includes `<state>`; a branch longer than `max_branch - state` is an
/// error (kev.serve's defaults are 8192 / 8192).
pub fn encode(
    tokenize: &dyn Fn(&str) -> Vec<u32>,
    record: &Record,
    special: &KevSpecial,
    max_state: usize,
    max_branch: usize,
    strict: bool,
) -> Result<Encoding, DecisionError> {
    let user = |text: &str| tokenize(&scrub_specials(text));
    let state_tokens = user(&record.state);
    if strict && state_tokens.len() + 1 > max_state {
        return Err(DecisionError::InvalidResponse(format!(
            "kev: state exceeds {max_state} tokens: {}",
            state_tokens.len() + 1
        )));
    }
    let mut state = vec![special.state];
    state.extend(state_tokens.iter().take(max_state.saturating_sub(1)));
    let len = state.len();
    let mut tokens = len;
    let mut branches = Vec::with_capacity(record.questions.len());
    for question in &record.questions {
        let mut ids = vec![special.q];
        ids.extend(user(&question.instr));
        let mut cursor = ids.len();
        let mut opts = Vec::with_capacity(question.options.len());
        for option in &question.options {
            let span_len = user(option).len() + 2;
            ids.push(special.opt);
            ids.extend(user(option));
            ids.push(special.opt_end);
            cursor += span_len;
            opts.push(cursor - 1);
        }
        ids.push(special.decide);
        if ids.len() > max_branch.saturating_sub(len) {
            return Err(DecisionError::InvalidResponse(format!(
                "kev: branch too long: {}",
                ids.len()
            )));
        }
        tokens += ids.len();
        let pos = (0..ids.len()).map(|i| (len + i) as u32).collect();
        branches.push(Branch {
            decide: ids.len() - 1,
            ids,
            pos,
            opts,
        });
    }
    Ok(Encoding {
        state,
        branches,
        state_truncated: state_tokens.len() + 1 > max_state,
        tokens,
    })
}

/// Kev's pointer head: `logit_k = (Wk h_opt_k + bk) · (Wq h_decide + bq) /
/// sqrt(dp)`, softmax over the options after dividing by the checkpoint
/// temperature. Weights are fp32; arithmetic is f64, as in kev.js.
#[derive(Debug, Clone)]
pub struct PointerHead {
    pub d: usize,
    pub dp: usize,
    qw: Vec<f32>,
    qb: Vec<f32>,
    kw: Vec<f32>,
    kb: Vec<f32>,
}

fn safetensor(
    header: &serde_json::Value,
    body: &[u8],
    name: &str,
) -> Result<(Vec<usize>, Vec<f32>), DecisionError> {
    let bad = |what: &str| DecisionError::InvalidResponse(format!("kev head: {name}: {what}"));
    let entry = header.get(name).ok_or_else(|| bad("missing"))?;
    if entry["dtype"] != "F32" {
        return Err(bad("expected F32"));
    }
    let shape: Vec<usize> = entry["shape"]
        .as_array()
        .ok_or_else(|| bad("no shape"))?
        .iter()
        .map(|v| v.as_u64().unwrap_or(0) as usize)
        .collect();
    let offsets = entry["data_offsets"]
        .as_array()
        .ok_or_else(|| bad("no offsets"))?;
    let (a, b) = (
        offsets[0].as_u64().unwrap_or(0) as usize,
        offsets[1].as_u64().unwrap_or(0) as usize,
    );
    let bytes = body.get(a..b).ok_or_else(|| bad("offsets out of range"))?;
    let data = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok((shape, data))
}

impl PointerHead {
    /// From `head.safetensors` bytes (`q.weight` `[dp, d]`, `q.bias`,
    /// `k.weight`, `k.bias`).
    pub fn from_safetensors(bytes: &[u8]) -> Result<Self, DecisionError> {
        let bad = |what: &str| DecisionError::InvalidResponse(format!("kev head: {what}"));
        let n = u64::from_le_bytes(
            bytes
                .get(..8)
                .ok_or_else(|| bad("truncated"))?
                .try_into()
                .unwrap(),
        ) as usize;
        let header: serde_json::Value =
            serde_json::from_slice(bytes.get(8..8 + n).ok_or_else(|| bad("truncated header"))?)
                .map_err(|e| bad(&e.to_string()))?;
        let body = &bytes[8 + n..];
        let (qshape, qw) = safetensor(&header, body, "q.weight")?;
        let (_, qb) = safetensor(&header, body, "q.bias")?;
        let (_, kw) = safetensor(&header, body, "k.weight")?;
        let (_, kb) = safetensor(&header, body, "k.bias")?;
        if qshape.len() != 2 {
            return Err(bad("q.weight is not 2-D"));
        }
        Ok(Self {
            dp: qshape[0],
            d: qshape[1],
            qw,
            qb,
            kw,
            kb,
        })
    }

    fn project(&self, w: &[f32], b: &[f32], h: &[f32]) -> Vec<f64> {
        (0..self.dp)
            .map(|i| {
                let row = &w[i * self.d..(i + 1) * self.d];
                let mut s = b[i] as f64;
                for (wj, hj) in row.iter().zip(h) {
                    s += *wj as f64 * *hj as f64;
                }
                s
            })
            .collect()
    }

    pub fn logits(&self, h_decide: &[f32], h_opts: &[&[f32]]) -> Vec<f64> {
        let q = self.project(&self.qw, &self.qb, h_decide);
        let scale = 1.0 / (self.dp as f64).sqrt();
        h_opts
            .iter()
            .map(|h| {
                let k = self.project(&self.kw, &self.kb, h);
                k.iter().zip(&q).map(|(a, b)| a * b).sum::<f64>() * scale
            })
            .collect()
    }

    pub fn probs(&self, h_decide: &[f32], h_opts: &[&[f32]], temperature: f64) -> Vec<f64> {
        let z: Vec<f64> = self
            .logits(h_decide, h_opts)
            .into_iter()
            .map(|x| x / temperature)
            .collect();
        let max = z.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let e: Vec<f64> = z.iter().map(|x| (x - max).exp()).collect();
        let sum: f64 = e.iter().sum();
        e.into_iter().map(|x| x / sum).collect()
    }
}

/// The fitted temperatures of the night-2 Qwen3.5 checkpoints, by upstream
/// revision (kev.js `NIGHT2_TEMPERATURE`).
const NIGHT2_TEMPERATURE: &[(&str, f64)] = &[
    (
        "225679690cdd1de6fceb1258b1bddf61c493cee9",
        2.406050072164233,
    ),
    (
        "54f4f8777356cd5bbbb6c6919c657f26e6f2f6d8",
        2.406050072164233,
    ),
    (
        "d842b1f6c7d8780d0686b9730381b422fe0308a0",
        2.406050072164233,
    ),
    (
        "4bc64c6b4c4881148661ffb823ce21fcfdc79a0e",
        2.1435469250725863,
    ),
    (
        "e226ccc58b9a4ae9fbbb272cf0d1d0ce6412fe54",
        2.1435469250725863,
    ),
    (
        "485ace8703592fcf405488b262449990824cfed1",
        2.1435469250725863,
    ),
    (
        "70dd4088ebf4eb82d15ef57a863a5b9a98b94d6c",
        2.1435469250725863,
    ),
    (
        "442e597d71840506c326c8c2f5eedd42aeac7bbd",
        2.2973967099940698,
    ),
    (
        "3cf1ab729d2b7bd678ec11cbbcdcb78c71fe4b95",
        2.2973967099940698,
    ),
    (
        "2629c06a5aeb0feb3b9783bafed17ed8f39ecf5c",
        2.2973967099940698,
    ),
    (
        "b54583620720cf4766b81100075f732b20789127",
        2.2973967099940698,
    ),
];

/// `temperatureFor`: the manifest's temperature, else the fitted value for
/// the run's revision, else 1 (raw).
pub fn temperature_for(run: &str, manifest_temperature: Option<f64>) -> f64 {
    if let Some(t) = manifest_temperature {
        return t;
    }
    let Some((_, rev)) = run.split_once('@') else {
        return 1.0;
    };
    let rev = rev.to_lowercase();
    if rev.is_empty() {
        return 1.0;
    }
    NIGHT2_TEMPERATURE
        .iter()
        .find(|(sha, _)| sha.starts_with(&rev) || rev.starts_with(&sha[..7]))
        .map(|(_, t)| *t)
        .unwrap_or(1.0)
}

/// Python's `round(x, digits)`: exact binary ties go to even.
pub fn py_round(x: f64, digits: i32) -> f64 {
    let half = x * 2f64.powi(digits + 1);
    if half.fract() == 0.0 && half.abs() % 2.0 == 1.0 {
        let f = (x * 10f64.powi(digits)).floor();
        let even = if f % 2.0 == 0.0 { f } else { f + 1.0 };
        return even / 10f64.powi(digits);
    }
    format!("{x:.prec$}", prec = digits as usize)
        .parse()
        .unwrap_or(x)
}

fn round_prob(x: f64) -> f64 {
    py_round(x, 4)
}

fn argmax(p: &[f64]) -> usize {
    p.iter()
        .enumerate()
        .fold(0, |best, (i, &v)| if v > p[best] { i } else { best })
}

/// `(p_max - 1/K) / (1 - 1/K)`; 1 for a single option.
pub fn choice_confidence(p: &[f64]) -> f64 {
    let k = p.len() as f64;
    if p.len() == 1 {
        return 1.0;
    }
    let max = p.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (max - 1.0 / k) / (1.0 - 1.0 / k)
}

/// `1 - E|level - mode| / (L - 1)`; 1 for a single level.
pub fn score_confidence(p: &[f64]) -> f64 {
    if p.len() == 1 {
        return 1.0;
    }
    let mode = argmax(p) as f64;
    1.0 - p
        .iter()
        .enumerate()
        .map(|(i, v)| v * (i as f64 - mode).abs())
        .sum::<f64>()
        / (p.len() as f64 - 1.0)
}

/// `toAnswers` for one question.
pub fn answer_from_probs(question: &Question, keys: &[OptionId], p: &[f64]) -> Answer {
    match question {
        Question::Noul(_) => Answer::Noul {
            noul: round_prob(p.get(1).copied().unwrap_or(0.0)),
        },
        Question::Choice(_) => Answer::Choice {
            choice: keys[argmax(p)].clone(),
            probabilities: keys
                .iter()
                .cloned()
                .zip(p.iter().map(|&v| round_prob(v)))
                .collect(),
            confidence: round_prob(choice_confidence(p)),
        },
        Question::Score(_) => Answer::Score {
            score: round_prob(p.iter().enumerate().map(|(j, v)| j as f64 * v).sum()),
            probabilities: p.iter().map(|&v| round_prob(v)).collect(),
            confidence: round_prob(score_confidence(p)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_print_like_python() {
        assert_eq!(py_float(0.1), "0.1");
        assert_eq!(py_float(1.5), "1.5");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(0.00001), "1e-05");
        assert_eq!(py_float(123456.789), "123456.789");
        assert_eq!(py_float(-2.5), "-2.5");
    }

    #[test]
    fn values_render_like_python_str() {
        let v = serde_json::json!({"a": true, "b": 2, "c": 2.5, "d": ["x", {"e": null}], "f": {"g": "h"}});
        assert_eq!(
            render(&v, 0),
            "a: True\nb: 2\nc: 2.5\nd:\n  - x\n  - e: \nf:\n  g: h"
        );
        assert_eq!(render(&serde_json::json!("plain"), 0), "plain");
    }

    #[test]
    fn delimiters_in_caller_text_are_defused() {
        assert_eq!(scrub_specials("a <|fim_prefix|> b"), "a <¦fim_prefix¦> b");
        assert_eq!(scrub_specials("<|box_end|><|x"), "<¦box_end¦><|x");
        assert_eq!(scrub_specials("no specials"), "no specials");
        assert_eq!(
            scrub_specials("<|a b|>"),
            "<|a b|>",
            "only [A-Za-z0-9_] names"
        );
    }

    fn special() -> KevSpecial {
        KevSpecial {
            state: 1,
            q: 2,
            opt: 3,
            opt_end: 4,
            decide: 5,
        }
    }

    #[test]
    fn branches_continue_the_state_and_index_their_markers() {
        let words = |t: &str| t.split_whitespace().map(|_| 9).collect::<Vec<u32>>();
        let record = Record {
            state: "one two three".into(),
            questions: vec![RecordQuestion {
                instr: "pick one".into(),
                options: vec!["a".into(), "b c".into()],
            }],
        };
        let enc = encode(&words, &record, &special(), 8192, 8192, false).expect("encodes");
        assert_eq!(enc.state, vec![1, 9, 9, 9]);
        let b = &enc.branches[0];
        assert_eq!(b.ids, vec![2, 9, 9, 3, 9, 4, 3, 9, 9, 4, 5]);
        assert_eq!(b.pos.first(), Some(&4), "positions continue the state");
        assert_eq!(b.decide, 10);
        assert_eq!(b.opts, vec![5, 9], "each option's </opt>");
        assert_eq!(enc.tokens, 4 + 11);
    }

    #[test]
    fn answers_and_confidences_follow_kev() {
        assert!((choice_confidence(&[0.5, 0.5]) - 0.0).abs() < 1e-12);
        assert!((choice_confidence(&[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!((score_confidence(&[0.0, 1.0, 0.0]) - 1.0).abs() < 1e-12);
        assert_eq!(py_round(0.03125, 4), 0.0312, "an exact tie goes to even");
        assert_eq!(py_round(0.12345, 4), 0.1235);
        assert_eq!(
            temperature_for("x/kev-0.8b@225679690cdd1de6", None),
            2.406050072164233
        );
        assert_eq!(
            temperature_for("x/kev-0.8b@2256796", None),
            2.406050072164233
        );
        assert_eq!(temperature_for("x/kev@unknown", None), 1.0);
        assert_eq!(temperature_for("x/kev@2256796", Some(1.7)), 1.7);
    }
}
