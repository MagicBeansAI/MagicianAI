//! How latency scales with state size, question count, and option count,
//! for the local models called directly (no engine, no socket). Runs with
//! `--features onnx`, `ORT_DYLIB_PATH`, and `LAYA_MODEL_DIR` and/or
//! `KEV_MODEL_DIR` (and, with `--features mlx`, `LAYA_MLX_DIR` / `KEV_MLX_DIR`); prints one line per cell, median of 7 runs after a
//! warm-up. Every run gets a fresh state (a changing number in it), so
//! Kev's state cache never helps — as in a game, where every frame is new.
#![cfg(feature = "onnx")]

use std::collections::BTreeMap;
use std::time::Instant;

use magician_decision::adapters::kev_onnx::{KevOnnxConfig, KevOnnxModel};
use magician_decision::adapters::laya_onnx::{LayaOnnxConfig, LayaOnnxModel};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, OptionId, Question, QuestionId,
};
use magician_decision::request::{DecisionRequest, DecisionState};
use magician_decision::StructuredDecisionModel;

/// A game-frame-like state of roughly `words` words, varied by `frame`.
fn state(words: usize, frame: usize) -> String {
    let mut text = format!(
        "Frame {frame}. The dinosaur is running; speed {}.",
        6 + frame % 5
    );
    let mut i = 0;
    while text.split_whitespace().count() < words {
        text.push_str(&format!(
            " Obstacle {i}: cactus at {} px ahead, height {} px.",
            120 + (frame * 7 + i * 31) % 400,
            20 + (i * 13) % 40
        ));
        i += 1;
    }
    text
}

fn request(words: usize, questions: usize, options: usize, frame: usize) -> DecisionRequest {
    let criteria: BTreeMap<OptionId, Criteria> = (0..options)
        .map(|o| {
            let label = match o {
                0 => "jump now".to_string(),
                1 => "keep running".to_string(),
                2 => "duck".to_string(),
                o => format!("move to lane {o}"),
            };
            (OptionId::new(format!("o{o}")), Criteria::Str(label))
        })
        .collect();
    DecisionRequest {
        operation: "sweep".into(),
        pack_id: "sweep".into(),
        pack_version: "1".into(),
        state: DecisionState::from_text(state(words, frame)),
        questions: (0..questions)
            .map(|q| {
                Question::Choice(ChoiceQuestion {
                    id: QuestionId::new(format!("q{q}")),
                    instructions: Instruction::Text(format!(
                        "What should the dinosaur do next to avoid obstacle {q}?"
                    )),
                    criteria: criteria.clone(),
                })
            })
            .collect(),
    }
}

/// `SWEEP_QUICK=1`: a few representative cells instead of the full grid.
fn cells(sizes: &[usize]) -> Vec<(usize, usize, usize)> {
    if std::env::var_os("SWEEP_QUICK").is_some() {
        return [
            (10, 1, 2),
            (60, 1, 8),
            (250, 1, 8),
            (250, 7, 8),
            (600, 7, 8),
            (2000, 1, 8),
        ]
        .into_iter()
        .filter(|(words, _, _)| sizes.contains(words))
        .collect();
    }
    let mut all = Vec::new();
    for &words in sizes {
        for questions in [1, 3, 7] {
            for options in [2, 8, 30] {
                all.push((words, questions, options));
            }
        }
    }
    all
}

async fn sweep(name: &str, model: &dyn StructuredDecisionModel, sizes: &[usize]) {
    let mut frame = 0;
    for (words, questions, options) in cells(sizes) {
        frame += 1;
        let warm = model
            .evaluate(request(words, questions, options, frame))
            .await;
        let Ok(warm) = warm else {
            eprintln!("[sweep] {name} words={words} q={questions} opts={options}: {warm:?}");
            continue;
        };
        let mut times = Vec::new();
        for _ in 0..7 {
            frame += 1;
            let started = Instant::now();
            model
                .evaluate(request(words, questions, options, frame))
                .await
                .expect("evaluates");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "[sweep] {name:8} tokens={:5} questions={questions} options={options:2}  median {:7.1} ms  min {:7.1} ms",
            warm.usage.input_tokens, times[3], times[0]
        );
    }
}

#[tokio::test]
async fn latency_by_state_questions_and_options() {
    if let Ok(dir) = std::env::var("LAYA_MODEL_DIR") {
        let model = LayaOnnxModel::load(LayaOnnxConfig::new(dir, "laya")).expect("laya loads");
        sweep("laya", &model, &[10, 60, 250, 600]).await;
    }
    if let Ok(dir) = std::env::var("KEV_MODEL_DIR") {
        let model = KevOnnxModel::load(KevOnnxConfig::new(dir, "kev")).expect("kev loads");
        sweep("kev-0.8b", &model, &[10, 60, 250, 600, 2000]).await;
    }
    #[cfg(feature = "mlx")]
    if let Ok(dir) = std::env::var("LAYA_MLX_DIR") {
        use magician_decision::adapters::laya_mlx::{LayaMlxConfig, LayaMlxModel};
        // Eager (the default) and compiled (`LayaMlxConfig::compile`).
        for compile in [false, true] {
            let mut config = LayaMlxConfig::new(dir.clone(), "laya-mlx");
            config.compile = compile;
            let model = LayaMlxModel::load(config).expect("laya-mlx loads");
            let name = if compile { "laya-mlx+c" } else { "laya-mlx" };
            sweep(name, &model, &[10, 60, 250, 600]).await;
        }
    }
    #[cfg(feature = "mlx")]
    if let Ok(dir) = std::env::var("KEV_MLX_DIR") {
        use magician_decision::adapters::kev_mlx::{KevMlxConfig, KevMlxModel};
        let model = KevMlxModel::load(KevMlxConfig::new(dir, "kev-mlx")).expect("kev-mlx loads");
        sweep("kev-mlx", &model, &[10, 60, 250, 600, 2000]).await;
    }
}
