//! laya — a bidirectional-encoder decision model (ModernBERT / mmBERT with a
//! typed decision head) — ported from the reference runtime
//! (`laya/common.py` `build_sequence`, `laya/onnx_agent.py` `_infer`,
//! upstream commit 1e28ac2). This file is the model-free half: how a
//! question becomes a token row and how the head's logits become typed
//! answers. The ONNX session lives in `laya_onnx.rs` behind the `onnx`
//! feature, so everything here is tested without a model.
//!
//! One row per question, all rows in one batched forward pass:
//! `[CLS] "<type> question: <instructions>" [SEP] ([MASK] option)… [SEP] state [SEP]`.
//! Each `[MASK]` is a marker the head scores; the answer is a softmax over
//! the markers.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::adapters::systemone::criteria_text;
use crate::error::DecisionError;
use crate::primitives::{OptionId, Question};
use crate::request::{Answer, DecisionRequest, DecisionState};

/// Question type ids the head was trained with.
pub const QTYPE_CHOICE: i64 = 0;
pub const QTYPE_SCORE: i64 = 1;
pub const QTYPE_NOUL: i64 = 2;

/// An option keeps at most this many tokens (the reference's `[:48]`).
const MAX_OPTION_TOKENS: usize = 48;
/// Temperatures outside this range are clamped (the reference refuses a
/// temperature that would sharpen a coin flip into a certainty).
const TEMP_MIN: f64 = 0.5;
const TEMP_MAX: f64 = 5.0;

/// `rl_agent_config.json`: the parts inference reads.
#[derive(Debug, Clone, Deserialize)]
pub struct LayaAgentConfig {
    #[serde(default = "default_max_len")]
    pub max_len: usize,
    #[serde(default = "default_head_max_len")]
    pub head_max_len: usize,
    #[serde(default = "default_temperature")]
    pub temperature: Vec<f64>,
    #[serde(default)]
    pub temperature_by_options: BTreeMap<String, f64>,
    /// Transformer layers in the decision head over the encoder.
    #[serde(default = "default_head_layers")]
    pub head_layers: usize,
}

fn default_head_layers() -> usize {
    2
}

fn default_max_len() -> usize {
    512
}
fn default_head_max_len() -> usize {
    192
}
fn default_temperature() -> Vec<f64> {
    vec![1.0, 1.0, 1.0]
}

/// The special tokens a row is built from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialTokens {
    pub cls: u32,
    pub sep: u32,
    pub mask: u32,
    pub pad: u32,
    /// The mask token's text, scrubbed from user text so it cannot forge a
    /// marker.
    pub mask_text: String,
}

/// One tokenized question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub ids: Vec<u32>,
    pub markers: Vec<usize>,
    pub qtype: i64,
}

fn qtype_name(question: &Question) -> &'static str {
    match question {
        Question::Choice(_) => "choice",
        Question::Score(_) => "score",
        Question::Noul(_) => "noul",
    }
}

pub fn qtype_of(question: &Question) -> i64 {
    match question {
        Question::Choice(_) => QTYPE_CHOICE,
        Question::Score(_) => QTYPE_SCORE,
        Question::Noul(_) => QTYPE_NOUL,
    }
}

/// Option texts in label order (`render_options`). Choice: `id` or
/// `id: criterion`; score: `level i: criterion`; noul: `[false, true]`
/// with the question's criteria or the reference defaults.
pub fn render_options(question: &Question) -> Vec<String> {
    match question {
        Question::Choice(choice) => choice
            .criteria
            .iter()
            .map(|(id, criterion)| {
                let text = criteria_text(criterion);
                if text.is_empty() {
                    id.as_str().to_string()
                } else {
                    format!("{}: {}", id.as_str(), text)
                }
            })
            .collect(),
        Question::Score(score) => score
            .levels
            .iter()
            .enumerate()
            .map(|(i, level)| format!("level {i}: {}", criteria_text(level)))
            .collect(),
        Question::Noul(noul) => {
            let (is_true, is_false) = match &noul.criteria {
                Some(criteria) => (
                    Some(criteria.is_true.as_str()),
                    criteria.is_false.as_deref(),
                ),
                None => (None, None),
            };
            let or = |text: Option<&str>, default: &str| {
                text.filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| default.to_string())
            };
            vec![
                format!("false: {}", or(is_false, "no, the statement does not hold")),
                format!("true: {}", or(is_true, "yes, the statement holds")),
            ]
        },
    }
}

/// The option ids a choice answer names, in label order.
pub fn choice_keys(question: &Question) -> Vec<OptionId> {
    match question {
        Question::Choice(choice) => choice.criteria.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// `json.dumps(value, ensure_ascii=False)`: Python's default separators
/// (`", "` and `": "`) and non-ASCII kept as is.
pub fn python_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(items) => format!(
            "[{}]",
            items.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        serde_json::Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    serde_json::Value::String(key.clone()),
                    python_json(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        scalar => scalar.to_string(),
    }
}

/// `serialize_state`: a string passes through, anything else is JSON.
pub fn serialize_state(state: &DecisionState) -> String {
    match state.as_json() {
        serde_json::Value::String(text) => text.clone(),
        other => python_json(other),
    }
}

/// `build_sequence`: one question's row, and the marker positions of its
/// options. `encode` tokenizes without special tokens. The parameters
/// mirror the reference function one for one, which is what parity reads.
#[allow(clippy::too_many_arguments)]
pub fn build_sequence(
    encode: &dyn Fn(&str) -> Vec<u32>,
    special: &SpecialTokens,
    state: &str,
    question_type: &str,
    instructions: &str,
    options: &[String],
    max_len: usize,
    head_max_len: usize,
) -> (Vec<u32>, Vec<usize>) {
    let scrub = |text: &str| text.replace(&special.mask_text, " ");
    let head_ids = encode(&format!(
        "{question_type} question: {}",
        scrub(instructions)
    ));
    let mut opt_ids: Vec<Vec<u32>> = options
        .iter()
        .map(|option| {
            let mut ids = vec![special.mask];
            ids.extend(
                encode(&format!(" {}", scrub(option)))
                    .into_iter()
                    .take(MAX_OPTION_TOKENS),
            );
            ids
        })
        .collect();
    let used = |opt_ids: &[Vec<u32>]| opt_ids.iter().map(Vec::len).sum::<usize>() as i64;
    let mut opt_budget = head_max_len as i64 - used(&opt_ids);
    if opt_budget < 16 {
        let per = 4.max(head_max_len.saturating_sub(16) / opt_ids.len().max(1));
        for ids in &mut opt_ids {
            ids.truncate(per);
        }
        opt_budget = head_max_len as i64 - used(&opt_ids);
    }
    let head_keep = 8.max(opt_budget).max(0) as usize;
    let mut ids = vec![special.cls];
    ids.extend(head_ids.into_iter().take(head_keep));
    ids.push(special.sep);
    let mut markers = Vec::with_capacity(opt_ids.len());
    for option in opt_ids {
        markers.push(ids.len());
        ids.extend(option);
    }
    ids.push(special.sep);
    let room = max_len.saturating_sub(ids.len() + 1);
    ids.extend(encode(&scrub(state)).into_iter().take(room));
    ids.push(special.sep);
    ids.truncate(max_len);
    markers.retain(|&marker| marker < max_len);
    (ids, markers)
}

/// Every question of a request as a row, in request order. A question whose
/// options no longer fit the head budget is an error, as in the reference.
pub fn build_rows(
    encode: &dyn Fn(&str) -> Vec<u32>,
    special: &SpecialTokens,
    config: &LayaAgentConfig,
    request: &DecisionRequest,
) -> Result<Vec<Row>, DecisionError> {
    let state = serialize_state(&request.state);
    request
        .questions
        .iter()
        .map(|question| {
            let options = render_options(question);
            let instructions = match question {
                Question::Choice(q) => q.instructions.as_text(),
                Question::Score(q) => q.instructions.as_text(),
                Question::Noul(q) => q.instructions.as_text(),
            };
            let (ids, markers) = build_sequence(
                encode,
                special,
                &state,
                qtype_name(question),
                &instructions,
                &options,
                config.max_len,
                config.head_max_len,
            );
            if markers.len() != options.len() {
                return Err(DecisionError::InvalidResponse(format!(
                    "question '{}' options exceed head_max_len={}",
                    question.id().as_str(),
                    config.head_max_len
                )));
            }
            Ok(Row {
                ids,
                markers,
                qtype: qtype_of(question),
            })
        })
        .collect()
}

/// The padded batch the graph takes: `input_ids`, `attention_mask`,
/// `marker_pos`, `marker_mask`, `qtype`, as flat row-major buffers.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub rows: usize,
    pub seq_len: usize,
    pub markers: usize,
    pub input_ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    pub marker_pos: Vec<i64>,
    pub marker_mask: Vec<bool>,
    pub qtype: Vec<i64>,
}

/// `collate_items`: pad to the longest row and the most markers.
pub fn collate(rows: &[Row], pad: u32) -> Batch {
    let seq_len = rows.iter().map(|r| r.ids.len()).max().unwrap_or(0);
    let markers = rows.iter().map(|r| r.markers.len()).max().unwrap_or(0);
    let mut batch = Batch {
        rows: rows.len(),
        seq_len,
        markers,
        input_ids: vec![pad as i64; rows.len() * seq_len],
        attention_mask: vec![0; rows.len() * seq_len],
        marker_pos: vec![0; rows.len() * markers],
        marker_mask: vec![false; rows.len() * markers],
        qtype: rows.iter().map(|r| r.qtype).collect(),
    };
    for (i, row) in rows.iter().enumerate() {
        for (j, &id) in row.ids.iter().enumerate() {
            batch.input_ids[i * seq_len + j] = id as i64;
            batch.attention_mask[i * seq_len + j] = 1;
        }
        for (j, &marker) in row.markers.iter().enumerate() {
            batch.marker_pos[i * markers + j] = marker as i64;
            batch.marker_mask[i * markers + j] = true;
        }
    }
    batch
}

fn clamp_temperature(t: f64) -> f64 {
    if !t.is_finite() {
        return 1.0;
    }
    t.clamp(TEMP_MIN, TEMP_MAX)
}

/// The temperature for a question of type `qtype` with `k` options: the
/// per-bucket fit when present, else the per-type one, clamped.
pub fn temperature(config: &LayaAgentConfig, qtype: i64, k: usize) -> f64 {
    let name = match qtype {
        QTYPE_CHOICE => "choice",
        QTYPE_SCORE => "score",
        _ => "noul",
    };
    let size = match k {
        0..=2 => "2",
        3..=5 => "3-5",
        6..=10 => "6-10",
        _ => "11+",
    };
    let bucket = format!("{name}:{size}");
    config
        .temperature_by_options
        .get(&bucket)
        .copied()
        .or_else(|| config.temperature.get(qtype as usize).copied())
        .map(clamp_temperature)
        .unwrap_or(1.0)
}

/// Softmax of `logits / t`.
pub fn softmax(logits: &[f32], t: f64) -> Vec<f64> {
    let scaled: Vec<f64> = logits.iter().map(|&z| z as f64 / t).collect();
    let max = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exp: Vec<f64> = scaled.iter().map(|z| (z - max).exp()).collect();
    let sum: f64 = exp.iter().sum();
    exp.into_iter().map(|e| e / sum).collect()
}

/// `confidence_from_probs`: `1 - H(p)/log k`, clipped to [0, 1].
pub fn entropy_confidence(p: &[f64]) -> f64 {
    let k = p.len();
    if k < 2 {
        return 1.0;
    }
    let entropy: f64 = -p.iter().map(|&x| x * x.clamp(1e-12, 1.0).ln()).sum::<f64>();
    (1.0 - entropy / (k as f64).ln()).clamp(0.0, 1.0)
}

/// Python's `round(x, 4)`: correctly rounded, ties to even.
pub fn round4(x: f64) -> f64 {
    format!("{x:.4}").parse().unwrap_or(x)
}

/// The typed answer for one question from its row's marker logits.
pub fn answer_from_logits(config: &LayaAgentConfig, question: &Question, logits: &[f32]) -> Answer {
    let qtype = qtype_of(question);
    let p = softmax(logits, temperature(config, qtype, logits.len()));
    let confidence = round4(entropy_confidence(&p));
    match question {
        Question::Choice(_) => {
            let keys = choice_keys(question);
            // numpy argmax: the first maximum wins.
            let best = p
                .iter()
                .enumerate()
                .fold(0, |best, (i, &v)| if v > p[best] { i } else { best });
            Answer::Choice {
                choice: keys[best].clone(),
                probabilities: keys.into_iter().zip(p.iter().map(|&v| round4(v))).collect(),
                confidence,
            }
        },
        Question::Score(_) => Answer::Score {
            score: round4(p.iter().enumerate().map(|(i, v)| i as f64 * v).sum()),
            probabilities: p.iter().map(|&v| round4(v)).collect(),
            confidence,
        },
        Question::Noul(_) => Answer::Noul {
            noul: round4(p.get(1).copied().unwrap_or(0.0)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{ChoiceQuestion, Criteria, Instruction, NoulQuestion, QuestionId};

    /// The reference tests' tokenizer: one id per whitespace-separated word.
    fn counting(text: &str) -> Vec<u32> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, _)| 1000 + i as u32)
            .collect()
    }

    fn special() -> SpecialTokens {
        SpecialTokens {
            cls: 101,
            sep: 102,
            mask: 103,
            pad: 0,
            mask_text: "[MASK]".to_string(),
        }
    }

    fn choice(pairs: &[(&str, &str)]) -> Question {
        Question::Choice(ChoiceQuestion {
            id: QuestionId::new("q"),
            instructions: Instruction::Text("pick".to_string()),
            criteria: pairs
                .iter()
                .map(|(k, v)| (OptionId::new(*k), Criteria::Str(v.to_string())))
                .collect(),
        })
    }

    #[test]
    fn options_render_like_the_reference() {
        // laya-ts parity: `{a: "yes", b: null}` -> ["a: yes", "b"].
        assert_eq!(
            render_options(&choice(&[("a", "yes"), ("b", "")])),
            vec!["a: yes", "b"]
        );
        let noul = Question::Noul(NoulQuestion {
            id: QuestionId::new("n"),
            instructions: Instruction::Text("x".to_string()),
            criteria: None,
        });
        assert_eq!(
            render_options(&noul),
            vec![
                "false: no, the statement does not hold",
                "true: yes, the statement holds"
            ]
        );
    }

    #[test]
    fn a_row_is_cls_head_sep_markers_sep_state_sep() {
        let options = render_options(&choice(&[("a", "x"), ("b", "y")]));
        let (ids, markers) = build_sequence(
            &counting,
            &special(),
            "hi",
            "choice",
            "pick",
            &options,
            64,
            32,
        );
        assert_eq!(ids[0], 101);
        assert_eq!(markers.len(), 2);
        assert_eq!(*ids.last().unwrap(), 102);
        for &m in &markers {
            assert_eq!(ids[m], 103, "each marker is a mask token");
        }
    }

    #[test]
    fn the_state_is_cut_from_the_right_to_the_room_left() {
        // laya-ts parity: max_len 16, head 8, 20-word state -> room 3, and
        // the head of the state is kept.
        let options = render_options(&choice(&[("a", "x"), ("b", "y")]));
        let state = (0..20)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let (ids, _) = build_sequence(
            &counting,
            &special(),
            &state,
            "choice",
            "pick",
            &options,
            16,
            8,
        );
        assert!(ids.len() <= 16);
        assert_eq!(&ids[12..15], &[1000, 1001, 1002]);
    }

    #[test]
    fn a_crowded_head_trims_options_evenly_and_keeps_every_marker() {
        let pairs: Vec<(String, String)> = (0..30)
            .map(|i| {
                (
                    format!("o{i}"),
                    "a long option label with many words".to_string(),
                )
            })
            .collect();
        let pairs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let options = render_options(&choice(&pairs));
        let (ids, markers) = build_sequence(
            &counting,
            &special(),
            "s",
            "choice",
            "pick",
            &options,
            1024,
            256,
        );
        assert_eq!(markers.len(), 30);
        // per = max(4, (256-16)/30) = 8 tokens per option, marker included.
        assert_eq!(markers[1] - markers[0], 8);
        assert!(ids.len() <= 1024);
    }

    #[test]
    fn a_mask_token_in_user_text_cannot_forge_a_marker() {
        let options = vec!["[MASK] sneaky".to_string(), "b".to_string()];
        let words = |text: &str| -> Vec<u32> {
            text.split_whitespace()
                .map(|w| if w == "[MASK]" { 103 } else { 7 })
                .collect()
        };
        let (ids, markers) = build_sequence(
            &words,
            &special(),
            "[MASK]",
            "choice",
            "[MASK]",
            &options,
            64,
            32,
        );
        let masks = ids.iter().filter(|&&id| id == 103).count();
        assert_eq!(masks, markers.len(), "only the option markers are masks");
    }

    #[test]
    fn collate_pads_like_the_reference() {
        let rows = vec![
            Row {
                ids: vec![1, 2, 3],
                markers: vec![1, 2],
                qtype: 0,
            },
            Row {
                ids: vec![4, 5],
                markers: vec![1],
                qtype: 1,
            },
        ];
        let batch = collate(&rows, 0);
        assert_eq!(batch.input_ids, vec![1, 2, 3, 4, 5, 0]);
        assert_eq!(batch.attention_mask, vec![1, 1, 1, 1, 1, 0]);
        assert_eq!(batch.marker_pos, vec![1, 2, 1, 0]);
        assert_eq!(batch.marker_mask, vec![true, true, true, false]);
        assert_eq!(batch.qtype, vec![0, 1]);
    }

    #[test]
    fn states_serialize_as_python_json() {
        let state =
            DecisionState::from_json(serde_json::json!({"body": "x", "n": [1, 2.5], "é": true}));
        let text = serialize_state(&state);
        assert!(text.contains("\"body\": \"x\""), "{text}");
        assert!(text.contains("\"n\": [1, 2.5]"), "{text}");
        assert!(text.contains("\"é\": true"), "non-ASCII kept: {text}");
        assert_eq!(serialize_state(&DecisionState::from_text("plain")), "plain");
    }

    #[test]
    fn temperature_prefers_the_bucket_and_clamps() {
        let config = LayaAgentConfig {
            max_len: 512,
            head_max_len: 192,
            temperature: vec![1.2, 1.0, 0.1],
            temperature_by_options: BTreeMap::from([("choice:11+".to_string(), 0.1006)]),
            head_layers: 2,
        };
        assert_eq!(temperature(&config, QTYPE_CHOICE, 3), 1.2);
        assert_eq!(
            temperature(&config, QTYPE_CHOICE, 20),
            0.5,
            "0.1006 clamps to 0.5"
        );
        assert_eq!(temperature(&config, QTYPE_NOUL, 2), 0.5);
    }

    #[test]
    fn answers_follow_the_reference_post_processing() {
        let config = LayaAgentConfig {
            max_len: 512,
            head_max_len: 192,
            temperature: vec![1.0, 1.0, 1.0],
            temperature_by_options: BTreeMap::new(),
            head_layers: 2,
        };
        let question = choice(&[("a", "x"), ("b", "y"), ("c", "z")]);
        let Answer::Choice {
            choice,
            probabilities,
            confidence,
        } = answer_from_logits(&config, &question, &[0.0, 2.0, 2.0])
        else {
            panic!("choice")
        };
        assert_eq!(choice.as_str(), "b", "the first of equal maxima");
        let sum: f64 = probabilities.values().sum();
        assert!((sum - 1.0).abs() < 1e-3);
        assert!(confidence > 0.0 && confidence < 1.0);
        let uniform = entropy_confidence(&[0.25, 0.25, 0.25, 0.25]);
        assert!(uniform.abs() < 1e-9, "a uniform answer has no confidence");
        // Python's round(): correctly rounded on the binary value.
        assert_eq!(round4(0.123_45), 0.1235);
        assert_eq!(round4(0.000_05), 0.0001);
    }
}
