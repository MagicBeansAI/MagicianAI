//! Kev on ONNX Runtime against kev.js's PyTorch fixtures. Runs only with
//! `--features onnx`, `KEV_MODEL_DIR` (a flattened kev.js bundle variant),
//! `KEV_FIXTURES` (kev.js `fixtures/kev-<size>.json`), and `ORT_DYLIB_PATH`
//! (ONNX Runtime >= 1.30); otherwise every test returns early.
//!
//! The bar is kev.js's own (`test/parity.ts`) for a quantized variant: the
//! worst probability difference at most twice the manifest's measured
//! `max_abs_dp`, the mean at most 0.02 over at least 20 questions, and an
//! answer may change only where the reference is a near-tie (<= 0.05).
#![cfg(feature = "onnx")]

use std::time::Instant;

use magician_decision::adapters::kev::{answer_from_probs, Record};
use magician_decision::adapters::kev_onnx::{KevOnnxConfig, KevOnnxModel};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::Answer;

const NEAR_TIE: f64 = 0.05;
const MEAN_ABS_DP: f64 = 0.02;

fn fixtures() -> Option<(KevOnnxModel, serde_json::Value, f64)> {
    let dir = std::env::var("KEV_MODEL_DIR").ok()?;
    let fixtures = std::env::var("KEV_FIXTURES").ok()?;
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{dir}/manifest.json")).unwrap())
            .unwrap();
    let max_abs_dp = manifest["variants"]["q8f32"]["parity"]["max_abs_dp"]
        .as_f64()
        .unwrap_or(0.1);
    let mut config = KevOnnxConfig::new(dir, "kev-0.8b-q8f32");
    config.max_state = 384;
    config.max_branch = 1024;
    let model = KevOnnxModel::load(config).expect("the bundle loads");
    let fixtures = serde_json::from_str(&std::fs::read_to_string(fixtures).unwrap()).unwrap();
    Some((model, fixtures, max_abs_dp))
}

fn argmax(p: &[f64]) -> usize {
    p.iter()
        .enumerate()
        .fold(0, |best, (i, &v)| if v > p[best] { i } else { best })
}

fn floats(value: &serde_json::Value) -> Vec<f64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect()
}

#[test]
fn encodings_and_probabilities_match_the_pytorch_fixtures() {
    let Some((model, fixtures, max_abs_dp)) = fixtures() else {
        return;
    };
    let (mut questions, mut worst, mut sum) = (0usize, 0f64, 0f64);
    let mut worst_at = String::new();
    let mut flips = Vec::new();
    let started = Instant::now();
    for fixture in fixtures["fixtures"].as_array().unwrap() {
        let name = fixture["name"].as_str().unwrap();
        let record: Record = serde_json::from_value(fixture["record"].clone()).unwrap();
        let expected = &fixture["encoding"];
        let encoding = model.encode(&record, true).expect("encodes");
        let ids: Vec<u32> = expected["ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let pos: Vec<u32> = expected["pos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let state_len = expected["state_len"].as_u64().unwrap() as usize;
        assert_eq!(encoding.state, ids[..state_len], "{name}: state tokens");
        let mut start = state_len;
        for (k, branch) in encoding.branches.iter().enumerate() {
            let decide = expected["decide_idx"][k].as_u64().unwrap() as usize;
            let end = decide + 1;
            assert_eq!(branch.ids, ids[start..end], "{name}: branch {k} tokens");
            assert_eq!(branch.pos, pos[start..end], "{name}: branch {k} positions");
            assert_eq!(branch.decide, decide - start, "{name}: branch {k} decide");
            let opts: Vec<usize> = expected["opt_idx"][k]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as usize - start)
                .collect();
            assert_eq!(branch.opts, opts, "{name}: branch {k} options");
            start = end;
        }
        assert_eq!(encoding.tokens, ids.len(), "{name}: token count");

        let probs = model.probs(&encoding, 1.0).expect("runs");
        for (k, got) in probs.iter().enumerate() {
            let reference = floats(&fixture["probs"][k]);
            let d = got
                .iter()
                .zip(&reference)
                .map(|(g, r)| (g - r).abs())
                .fold(0f64, f64::max);
            questions += 1;
            sum += d;
            if d > worst {
                worst = d;
                worst_at = format!("{name} q{k}");
            }
            let (a, b) = (argmax(&reference), argmax(got));
            if a != b {
                flips.push((format!("{name} q{k}"), reference[a] - reference[b]));
            }
        }
    }
    let mean = sum / questions as f64;
    eprintln!(
        "[kev-onnx] {questions} questions in {:?}: mean |dp| {mean:.2e}, max {worst:.2e} ({worst_at}); flips {flips:?}",
        started.elapsed()
    );
    assert!(
        worst <= 2.0 * max_abs_dp,
        "max |dp| {worst} at {worst_at} > {}",
        2.0 * max_abs_dp
    );
    assert!(
        questions >= 20 && mean <= MEAN_ABS_DP,
        "mean |dp| {mean} over {questions}"
    );
    for (at, margin) in &flips {
        assert!(
            *margin <= NEAR_TIE,
            "answer changed at {at}, reference margin {margin}"
        );
    }
}

/// The answer formulas, fed the fixtures' reference probabilities, give the
/// fixtures' answers exactly (kev.api.to_answers, 4-place rounding).
#[test]
fn answer_shaping_matches_the_fixtures() {
    let Some((_, fixtures, _)) = fixtures() else {
        return;
    };
    let mut checked = 0;
    for fixture in fixtures["fixtures"].as_array().unwrap() {
        for (k, meta) in fixture["meta"].as_array().unwrap().iter().enumerate() {
            let id = meta["id"].as_str().unwrap();
            let keys: Vec<OptionId> = meta["keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| OptionId::new(v.as_str().unwrap()))
                .collect();
            let instructions = Instruction::Text(String::new());
            let question = match meta["type"].as_str().unwrap() {
                "choice" => Question::Choice(ChoiceQuestion {
                    id: QuestionId::new(id),
                    instructions,
                    criteria: keys
                        .iter()
                        .map(|k| (k.clone(), Criteria::Str(String::new())))
                        .collect(),
                }),
                "score" => Question::Score(ScoreQuestion {
                    id: QuestionId::new(id),
                    instructions,
                    levels: keys.iter().map(|_| Criteria::Str(String::new())).collect(),
                }),
                _ => Question::Noul(NoulQuestion {
                    id: QuestionId::new(id),
                    instructions,
                    criteria: None,
                }),
            };
            let reference = floats(&fixture["probs"][k]);
            let expected = &fixture["answers"][id];
            match answer_from_probs(&question, &keys, &reference) {
                Answer::Noul { noul } => assert_eq!(noul, expected["noul"].as_f64().unwrap()),
                Answer::Choice {
                    choice,
                    probabilities,
                    confidence,
                } => {
                    assert_eq!(
                        choice.as_str(),
                        expected["choice"].as_str().unwrap(),
                        "{id}"
                    );
                    assert_eq!(confidence, expected["confidence"].as_f64().unwrap(), "{id}");
                    for (key, p) in probabilities {
                        assert_eq!(p, expected["probabilities"][key.as_str()].as_f64().unwrap());
                    }
                },
                Answer::Score {
                    score,
                    probabilities,
                    confidence,
                } => {
                    assert_eq!(score, expected["score"].as_f64().unwrap(), "{id}");
                    assert_eq!(confidence, expected["confidence"].as_f64().unwrap(), "{id}");
                    for (j, p) in probabilities.iter().enumerate() {
                        assert_eq!(
                            *p,
                            expected["probabilities"][j.to_string()].as_f64().unwrap()
                        );
                    }
                },
            }
            checked += 1;
        }
    }
    eprintln!("[kev-onnx] answer shaping: {checked} answers match");
    assert!(checked > 0);
}
