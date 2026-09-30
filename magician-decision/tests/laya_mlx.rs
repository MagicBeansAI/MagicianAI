//! The Rust MLX port of laya against laya-mlx (Python MLX) on the original
//! checkpoint, pinned to the revision setup-decision-models installs. Runs
//! only with `--features mlx` and `LAYA_MLX_DIR` (a laya-multilingual-mlx or
//! laya-english-mlx model directory). Cases and references default to
//! `tests/fixtures/laya/` (`LAYA_PARITY_CASES` / `LAYA_MLX_REFERENCE`
//! override); the reference is picked from the checkpoint's encoder.
//! `scripts/laya-mlx-reference.py` regenerates them: rows, raw logits in
//! float32 and float16, and predict()'s probabilities.
//!
//! Bars: identical token ids and marker positions, and both precisions
//! measured against laya-mlx's float32 run, the reference precision:
//! float32 logits within 2e-3 and probabilities within 1e-3; float16
//! logits within 1e-2 and probabilities within 5e-3.
//!
//! The float16 bar was first set against laya-mlx's own float16 run (1e-2)
//! and failed at 1.26e-2: that compared two float16 roundings (laya-mlx
//! compiles its forward pass, fusing ops) with each other. Against the
//! float32 reference this port's float16 is off by 3.3e-3 and laya-mlx's
//! by 1.17e-2, so float16 is judged by its distance from float32, and the
//! log reports laya-mlx's own for comparison.
#![cfg(feature = "mlx")]

use magician_decision::adapters::laya_mlx::{LayaDtype, LayaMlxConfig, LayaMlxModel};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::{Answer, DecisionRequest, DecisionState};
use magician_decision::StructuredDecisionModel;

fn request_of(case: &serde_json::Value) -> DecisionRequest {
    let questions: Vec<Question> = case["questions"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(qid, q)| {
            let id = QuestionId::new(qid.as_str());
            let instructions = Instruction::Text(q["instructions"].as_str().unwrap().into());
            match q["type"].as_str().unwrap() {
                "choice" => Question::Choice(ChoiceQuestion {
                    id,
                    instructions,
                    criteria: q["criteria"]
                        .as_object()
                        .unwrap()
                        .iter()
                        .map(|(k, v)| {
                            (
                                OptionId::new(k.as_str()),
                                Criteria::Str(v.as_str().unwrap().into()),
                            )
                        })
                        .collect(),
                }),
                "score" => Question::Score(ScoreQuestion {
                    id,
                    instructions,
                    levels: q["criteria"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| Criteria::Str(v.as_str().unwrap().into()))
                        .collect(),
                }),
                _ => Question::Noul(NoulQuestion {
                    id,
                    instructions,
                    criteria: q.get("criteria").map(|c| {
                        magician_decision::primitives::NoulCriteria {
                            is_true: c["true"].as_str().unwrap().into(),
                            is_false: c.get("false").and_then(|v| v.as_str()).map(str::to_string),
                        }
                    }),
                }),
            }
        })
        .collect();
    DecisionRequest {
        operation: "parity".into(),
        pack_id: "parity".into(),
        pack_version: "1".into(),
        state: match &case["state"] {
            serde_json::Value::String(text) => DecisionState::from_text(text.clone()),
            other => DecisionState::from_json(other.clone()),
        },
        questions,
    }
}

fn floats(value: &serde_json::Value) -> Vec<f64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect()
}

async fn check(dtype: LayaDtype, compile: bool, key: &str, logit_bar: f64, prob_bar: f64) {
    let Ok(dir) = std::env::var("LAYA_MLX_DIR") else {
        return;
    };
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/laya");
    let cases =
        std::env::var("LAYA_PARITY_CASES").unwrap_or_else(|_| format!("{fixtures}/cases_mlx.json"));
    // The checkpoint's reference: its encoder says which laya it is.
    let reference = std::env::var("LAYA_MLX_REFERENCE").unwrap_or_else(|_| {
        let agent: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{dir}/rl_agent_config.json")).unwrap(),
        )
        .unwrap();
        let english = agent["encoder"]
            .as_str()
            .is_some_and(|e| e.contains("ModernBERT"));
        let which = if english { "english" } else { "multilingual" };
        format!("{fixtures}/mlx_reference_{which}.json")
    });
    let mut config = LayaMlxConfig::new(dir, "laya-multilingual-mlx");
    config.dtype = dtype;
    config.compile = compile;
    let started = std::time::Instant::now();
    let model = LayaMlxModel::load(config).expect("loads");
    let key_label = if compile {
        format!("{key}, compiled")
    } else {
        key.to_string()
    };
    eprintln!("[laya-mlx {key_label}] loaded in {:?}", started.elapsed());
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(cases).unwrap()).unwrap();
    let reference: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(reference).unwrap()).unwrap();
    // Our distance from laya-mlx's float32 run, and laya-mlx's own at this
    // precision (for the log).
    let (mut compared, mut worst_logit, mut worst_prob, mut theirs) = (0, 0f64, 0f64, 0f64);
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let request = request_of(case);
        let rows = model.rows(&request).expect("rows");
        let logits = model.logits_for_rows(rows.clone()).await.expect("runs");
        let response = model.evaluate(request.clone()).await.expect("evaluates");
        for ((question, row), got) in request.questions.iter().zip(&rows).zip(&logits) {
            let qid = question.id().as_str();
            let expected = &reference[name][qid];
            let ids: Vec<u32> = floats(&expected["ids"]).iter().map(|&v| v as u32).collect();
            if row.ids != ids {
                let first = row.ids.iter().zip(&ids).position(|(a, b)| a != b);
                let at = first.unwrap_or(row.ids.len().min(ids.len()));
                let lo = at.saturating_sub(6);
                panic!(
                    "{name}/{qid}: token ids differ at {at} (ours {} tokens, laya-mlx {}): ours {:?} vs {:?}; ours end {:?} vs {:?}",
                    row.ids.len(),
                    ids.len(),
                    &row.ids[lo..(at + 12).min(row.ids.len())],
                    &ids[lo..(at + 12).min(ids.len())],
                    &row.ids[row.ids.len().saturating_sub(12)..],
                    &ids[ids.len().saturating_sub(12)..],
                );
            }
            let markers: Vec<usize> = floats(&expected["markers"])
                .iter()
                .map(|&v| v as usize)
                .collect();
            assert_eq!(
                row.markers, markers,
                "{name}/{qid}: marker positions differ"
            );
            let truth = floats(&expected["logits_float32"]);
            for (g, t) in got.iter().zip(&truth) {
                worst_logit = worst_logit.max((*g as f64 - t).abs());
            }
            for (r, t) in floats(&expected[format!("logits_{key}")])
                .iter()
                .zip(&truth)
            {
                theirs = theirs.max((r - t).abs());
            }
            let probs: Vec<f64> = match &response.answers[question.id()] {
                Answer::Choice { probabilities, .. } => probabilities.values().copied().collect(),
                Answer::Score { probabilities, .. } => probabilities.clone(),
                Answer::Noul { noul } => {
                    vec![magician_decision::adapters::laya::round4(1.0 - noul), *noul]
                },
            };
            for (g, w) in probs.iter().zip(floats(&expected["probabilities_float32"])) {
                worst_prob = worst_prob.max((g - w).abs());
            }
            compared += 1;
        }
    }
    eprintln!(
        "[laya-mlx {key_label}] {compared} questions vs laya-mlx float32: max |logit diff| {worst_logit:.2e}, max |prob diff| {worst_prob:.2e} (laya-mlx {key} itself: {theirs:.2e})"
    );
    assert!(compared > 0);
    assert!(
        worst_logit <= logit_bar,
        "logits off by {worst_logit} (bar {logit_bar})"
    );
    assert!(
        worst_prob <= prob_bar,
        "probabilities off by {worst_prob} (bar {prob_bar})"
    );
}

#[tokio::test]
async fn matches_laya_mlx_in_float32() {
    check(LayaDtype::Float32, false, "float32", 2e-3, 1e-3).await;
}

#[tokio::test]
async fn float16_stays_near_float32() {
    check(LayaDtype::Float16, false, "float16", 1e-2, 5e-3).await;
}

/// The compiled path (`LayaMlxConfig::compile`, lengths padded to a
/// multiple of 64) meets the float32 bar too: padding is masked out.
#[tokio::test]
async fn compiled_matches_laya_mlx_in_float32() {
    check(LayaDtype::Float32, true, "float32", 2e-3, 1e-3).await;
}
