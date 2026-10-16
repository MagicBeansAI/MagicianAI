//! Kev on MLX against kev.js's PyTorch fixtures — the same fixtures and
//! bar the ONNX adapter meets (`tests/kev_onnx.rs`). Runs only with
//! `--features mlx`, `KEV_MLX_DIR` (a kev-mlx model directory), and
//! `KEV_FIXTURES`; otherwise returns early.
//!
//! The record comes from each fixture, so this checks what kev-mlx adds:
//! kev-core's encoding of our record and the MLX backbone and head. The
//! bar: the worst probability difference at most `MAX_ABS_DP`, the mean at
//! most 0.02 over at least 20 questions, and an answer may change only at
//! a near-tie (reference margin <= 0.05).
#![cfg(feature = "mlx")]

use std::time::Instant;

use magician_decision::adapters::kev::Record;
use magician_decision::adapters::kev_mlx::{KevMlxConfig, KevMlxModel};

const NEAR_TIE: f64 = 0.05;
const MEAN_ABS_DP: f64 = 0.02;
/// Twice the ONNX q8 variant's measured worst case (kev.js manifest,
/// 0.0967): MLX runs the fp32-merged weights, so it should sit well inside.
const MAX_ABS_DP: f64 = 0.19;

fn argmax(p: &[f64]) -> usize {
    p.iter()
        .enumerate()
        .fold(0, |best, (i, &v)| if v > p[best] { i } else { best })
}

#[tokio::test]
async fn probabilities_match_the_pytorch_fixtures() {
    let (Ok(dir), Ok(fixtures)) = (std::env::var("KEV_MLX_DIR"), std::env::var("KEV_FIXTURES"))
    else {
        return;
    };
    let started = Instant::now();
    // The fixtures hold raw probabilities (temperature 1), as the ONNX test
    // compares them; serving uses the checkpoint's fitted temperature.
    let mut config = KevMlxConfig::new(dir, "kev-0.8b-mlx");
    config.temperature = Some(1.0);
    let model = KevMlxModel::load(config).expect("loads");
    eprintln!("[kev-mlx] loaded in {:?}", started.elapsed());
    let fixtures: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixtures).unwrap()).unwrap();
    let (mut questions, mut worst, mut sum) = (0usize, 0f64, 0f64);
    let mut worst_at = String::new();
    let mut flips = Vec::new();
    let started = Instant::now();
    for fixture in fixtures["fixtures"].as_array().unwrap() {
        let name = fixture["name"].as_str().unwrap();
        let record: Record = serde_json::from_value(fixture["record"].clone()).unwrap();
        let (probs, tokens) = model.probs_for_record(record).await.expect("runs");
        let expected = fixture["encoding"]["ids"].as_array().unwrap().len() as u64;
        assert_eq!(tokens, expected, "{name}: token count");
        for (k, got) in probs.iter().enumerate() {
            let reference: Vec<f64> = fixture["probs"][k]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
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
        "[kev-mlx] {questions} questions in {:?}: mean |dp| {mean:.2e}, max {worst:.2e} ({worst_at}); flips {flips:?}",
        started.elapsed()
    );
    assert!(worst <= MAX_ABS_DP, "max |dp| {worst} at {worst_at}");
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
