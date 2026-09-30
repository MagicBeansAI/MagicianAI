//! MLX kernel timings from Rust, to compare with the same ops from Python
//! (`MLX_MICROBENCH=1`). Diagnostic only.
#![cfg(feature = "mlx")]

use mlx_rs::{fast, ops, random, transforms::eval, Array, Dtype};

fn time(label: &str, mut f: impl FnMut() -> Array) {
    for _ in 0..5 {
        eval([&f()]).unwrap();
    }
    let started = std::time::Instant::now();
    for _ in 0..50 {
        eval([&f()]).unwrap();
    }
    eprintln!(
        "[mlx-rs] {label}: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0 / 50.0
    );
}

#[test]
fn kernels() {
    if std::env::var_os("MLX_MICROBENCH").is_none() {
        return;
    }
    let h = |shape: &[i32]| {
        random::normal::<f32>(shape, None, None, None)
            .unwrap()
            .as_dtype(Dtype::Float16)
            .unwrap()
    };
    let x = h(&[7, 1024, 768]);
    let w = h(&[2304, 768]);
    time("matmul 7x1024x768 @ 768x2304", || {
        ops::matmul(&x, ops::swap_axes(&w, -1, -2).unwrap()).unwrap()
    });
    let (q, k, v) = (
        h(&[7, 12, 1024, 64]),
        h(&[7, 12, 1024, 64]),
        h(&[7, 12, 1024, 64]),
    );
    let mask = Array::from_slice(&vec![true; 7 * 1024], &[7, 1, 1, 1024]);
    time("sdpa 7x12x1024x64 bool key mask", || {
        fast::scaled_dot_product_attention(&q, &k, &v, 0.125, &mask, None).unwrap()
    });
    let full = Array::from_slice(&vec![true; 7 * 1024 * 1024], &[7, 1, 1024, 1024]);
    time("sdpa 7x12x1024x64 bool full mask", || {
        fast::scaled_dot_product_attention(&q, &k, &v, 0.125, &full, None).unwrap()
    });
    let small = h(&[1, 64, 768]);
    time("matmul 1x64x768 @ 768x2304", || {
        ops::matmul(&small, ops::swap_axes(&w, -1, -2).unwrap()).unwrap()
    });
}

/// Where one small laya-mlx call spends its time (`LAYA_MLX_DIR`).
#[tokio::test]
async fn laya_call_phases() {
    use magician_decision::adapters::laya_mlx::{LayaMlxConfig, LayaMlxModel};
    use magician_decision::primitives::{
        ChoiceQuestion, Criteria, Instruction, OptionId, Question, QuestionId,
    };
    use magician_decision::request::{DecisionRequest, DecisionState};
    use magician_decision::StructuredDecisionModel;
    let (Some(_), Ok(dir)) = (
        std::env::var_os("MLX_MICROBENCH"),
        std::env::var("LAYA_MLX_DIR"),
    ) else {
        return;
    };
    let model = LayaMlxModel::load(LayaMlxConfig::new(dir, "laya")).unwrap();
    let request = DecisionRequest {
        operation: "t".into(),
        pack_id: "t".into(),
        pack_version: "1".into(),
        state: DecisionState::from_text("Frame 1. The dinosaur is running; speed 7. Obstacle 0: cactus at 127 px ahead, height 20 px."),
        questions: vec![Question::Choice(ChoiceQuestion {
            id: QuestionId::new("q"),
            instructions: Instruction::Text("What should the dinosaur do next?".into()),
            criteria: [("jump", "jump now"), ("run", "keep running")]
                .into_iter()
                .map(|(k, v)| (OptionId::new(k), Criteria::Str(v.into())))
                .collect(),
        })],
    };
    for _ in 0..5 {
        model.evaluate(request.clone()).await.unwrap();
    }
    let n = 50;
    let (mut rows_t, mut logits_t, mut eval_t) = (0.0, 0.0, 0.0);
    for _ in 0..n {
        let t = std::time::Instant::now();
        let rows = model.rows(&request).unwrap();
        rows_t += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        model.logits_for_rows(rows).await.unwrap();
        logits_t += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        model.evaluate(request.clone()).await.unwrap();
        eval_t += t.elapsed().as_secs_f64();
    }
    let ms = |s: f64| s * 1000.0 / n as f64;
    eprintln!(
        "[laya-phases] rows {:.3} ms, logits (worker round trip) {:.3} ms, evaluate {:.3} ms",
        ms(rows_t),
        ms(logits_t),
        ms(eval_t)
    );
}

#[test]
fn small_kernels() {
    if std::env::var_os("MLX_MICROBENCH").is_none() {
        return;
    }
    let h = |shape: &[i32]| {
        random::normal::<f32>(shape, None, None, None)
            .unwrap()
            .as_dtype(Dtype::Float16)
            .unwrap()
    };
    let x = h(&[1, 64, 768]);
    let (wqkv, bqkv, wi) = (h(&[2304, 768]), h(&[2304]), h(&[2304, 768]));
    let lnw = h(&[768]);
    let (q, k, v) = (
        h(&[1, 12, 64, 64]),
        h(&[1, 12, 64, 64]),
        h(&[1, 12, 64, 64]),
    );
    let key_mask = Array::from_slice(&[true; 64], &[1, 1, 1, 64]);
    let local_mask = Array::from_slice(&vec![true; 64 * 64], &[1, 1, 64, 64]);
    time("small matmul 1x64x768 @ 768x2304", || {
        ops::matmul(&x, ops::swap_axes(&wqkv, -1, -2).unwrap()).unwrap()
    });
    time("small addmm 1x64x768 @ 768x2304 + b", || {
        ops::addmm(
            &bqkv,
            &x,
            ops::swap_axes(&wqkv, -1, -2).unwrap(),
            None,
            None,
        )
        .unwrap()
    });
    time("small layer_norm 1x64x768", || {
        fast::layer_norm(&x, &lnw, None, 1e-5).unwrap()
    });
    time("small rope 1x12x64x64", || {
        fast::rope(&q, 64, false, 160000.0, 1.0, 0, None).unwrap()
    });
    time("small sdpa key mask", || {
        fast::scaled_dot_product_attention(&q, &k, &v, 0.125, &key_mask, None).unwrap()
    });
    time("small sdpa full LxL mask", || {
        fast::scaled_dot_product_attention(&q, &k, &v, 0.125, &local_mask, None).unwrap()
    });
    let y = h(&[1, 64, 2304]);
    time("small gelu 1x64x1152 * gate", || {
        let p = y.split_equal(2, -1).unwrap();
        mlx_rs::nn::gelu(&p[0]).unwrap().multiply(&p[1]).unwrap()
    });
    let _ = wi;
}
