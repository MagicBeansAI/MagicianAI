//! laya on ONNX Runtime against a real model directory. Runs only with
//! `--features onnx` and `LAYA_MODEL_DIR` set (the files are downloaded,
//! not committed); otherwise every test returns early.
//!
//! There are no published ONNX fixtures to match to the digit, so these
//! assert what a working model must get right — unambiguous questions in
//! two languages, every answer well-formed — and report latency.
#![cfg(feature = "onnx")]

use std::collections::BTreeMap;
use std::time::Instant;

use magician_decision::adapters::laya_onnx::{LayaOnnxConfig, LayaOnnxModel};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::{Answer, DecisionRequest, DecisionState};
use magician_decision::StructuredDecisionModel;

fn model() -> Option<LayaOnnxModel> {
    let dir = std::env::var("LAYA_MODEL_DIR").ok()?;
    Some(
        LayaOnnxModel::load(LayaOnnxConfig::new(dir, "laya-multilingual-int8"))
            .expect("the model directory loads"),
    )
}

fn sentiment(text: &str) -> DecisionRequest {
    let criteria: BTreeMap<OptionId, Criteria> = [
        ("positive", "the writer is pleased or happy"),
        ("negative", "the writer is unhappy, angry, or disappointed"),
        ("neutral", "no clear feeling either way"),
    ]
    .into_iter()
    .map(|(k, v)| (OptionId::new(k), Criteria::Str(v.to_string())))
    .collect();
    DecisionRequest {
        operation: "t".into(),
        pack_id: "t".into(),
        pack_version: "1".into(),
        state: DecisionState::from_text(text),
        questions: vec![Question::Choice(ChoiceQuestion {
            id: QuestionId::new("sentiment"),
            instructions: Instruction::Text("What is the sentiment of the message?".into()),
            criteria,
        })],
    }
}

fn choice_of(model_answer: &Answer) -> &str {
    match model_answer {
        Answer::Choice { choice, .. } => choice.as_str(),
        other => panic!("expected a choice, got {other:?}"),
    }
}

#[tokio::test]
async fn clear_sentiment_in_two_languages() {
    let Some(model) = model() else { return };
    for (text, expected) in [
        (
            "I absolutely love this, thank you so much! Best day ever.",
            "positive",
        ),
        (
            "This is terrible. It broke on day one and support ignored me.",
            "negative",
        ),
        (
            "¡Me encanta! Funciona perfectamente, muchas gracias.",
            "positive",
        ),
        (
            "Es un desastre, estoy muy decepcionado con el servicio.",
            "negative",
        ),
    ] {
        let response = model.evaluate(sentiment(text)).await.expect("evaluates");
        let answer = &response.answers[&QuestionId::new("sentiment")];
        assert_eq!(choice_of(answer), expected, "{text:?} -> {answer:?}");
    }
}

#[tokio::test]
async fn nouls_and_scores_are_well_formed_and_directional() {
    let Some(model) = model() else { return };
    let noul = |id: &str, instructions: &str| {
        Question::Noul(NoulQuestion {
            id: QuestionId::new(id),
            instructions: Instruction::Text(instructions.into()),
            criteria: None,
        })
    };
    let request = DecisionRequest {
        operation: "t".into(),
        pack_id: "t".into(),
        pack_version: "1".into(),
        state: DecisionState::from_json(serde_json::json!({
            "request": "Book a table for two at an Italian restaurant tonight",
            "memory": "The user is allergic to peanuts and prefers Italian food."
        })),
        questions: vec![
            noul(
                "relevant",
                "`memory` is relevant context for answering `request`",
            ),
            noul("about_cars", "`request` is about buying a car"),
            Question::Score(ScoreQuestion {
                id: QuestionId::new("urgency"),
                instructions: Instruction::Text("How urgent is `request`?".into()),
                levels: vec![
                    Criteria::Str("not urgent: weeks away".into()),
                    Criteria::Str("soon: within days".into()),
                    Criteria::Str("urgent: today".into()),
                ],
            }),
        ],
    };
    let started = Instant::now();
    let response = model.evaluate(request).await.expect("evaluates");
    let elapsed = started.elapsed();
    let noul_of = |id: &str| match &response.answers[&QuestionId::new(id)] {
        Answer::Noul { noul } => *noul,
        other => panic!("expected a noul, got {other:?}"),
    };
    // The reference runtime answers `relevant` at 0.4238 on this phrasing
    // (a model judgment, pinned by the parity test); only the clear-cut
    // direction is asserted here.
    assert!(
        noul_of("about_cars") < 0.1,
        "about_cars: {}",
        noul_of("about_cars")
    );
    match &response.answers[&QuestionId::new("urgency")] {
        Answer::Score {
            score,
            probabilities,
            confidence,
        } => {
            assert_eq!(probabilities.len(), 3);
            assert!((0.0..=2.0).contains(score));
            assert!((0.0..=1.0).contains(confidence));
        },
        other => panic!("expected a score, got {other:?}"),
    }
    assert!(response.usage.input_tokens > 0);
    eprintln!(
        "[laya-onnx] 3 questions in {:?} ({} input tokens): relevant={} about_cars={} urgency={:?}",
        elapsed,
        response.usage.input_tokens,
        noul_of("relevant"),
        noul_of("about_cars"),
        response.answers[&QuestionId::new("urgency")]
    );
}

#[tokio::test]
async fn latency_on_this_machine() {
    let Some(model) = model() else { return };
    let request = sentiment("The package arrived late but the product itself is fine.");
    let _ = model.evaluate(request.clone()).await.expect("warm-up");
    let runs = 10;
    let started = Instant::now();
    for _ in 0..runs {
        model.evaluate(request.clone()).await.expect("evaluates");
    }
    eprintln!(
        "[laya-onnx] one choice question: {:?} per request over {runs} runs",
        started.elapsed() / runs
    );
}

/// Parity with laya's own Python runtime (`build_sequence` + ONNX Runtime +
/// the reference post-processing) on a shared case file: the same token
/// ids, and every probability and confidence equal to 4 places. Cases and
/// reference default to `tests/fixtures/laya/` (`LAYA_PARITY_CASES` /
/// `LAYA_PARITY_REFERENCE` override); `scripts/laya-onnx-reference.py`
/// regenerates them.
#[tokio::test]
async fn matches_the_reference_runtime() {
    let Some(model) = model() else {
        return;
    };
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/laya");
    let cases =
        std::env::var("LAYA_PARITY_CASES").unwrap_or_else(|_| format!("{fixtures}/cases.json"));
    let reference = std::env::var("LAYA_PARITY_REFERENCE")
        .unwrap_or_else(|_| format!("{fixtures}/onnx_reference.json"));
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(cases).unwrap()).unwrap();
    let reference: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(reference).unwrap()).unwrap();
    let dir = std::env::var("LAYA_MODEL_DIR").unwrap();
    let tokenizer = tokenizers::Tokenizer::from_file(format!("{dir}/tokenizer.json")).unwrap();
    let tcfg: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{dir}/tokenizer_config.json")).unwrap(),
    )
    .unwrap();
    let id = |key: &str| tokenizer.token_to_id(tcfg[key].as_str().unwrap()).unwrap();
    let special = magician_decision::adapters::laya::SpecialTokens {
        cls: id("cls_token"),
        sep: id("sep_token"),
        mask: id("mask_token"),
        pad: id("pad_token"),
        mask_text: tcfg["mask_token"].as_str().unwrap().to_string(),
    };
    let encode = |text: &str| tokenizer.encode(text, false).unwrap().get_ids().to_vec();
    let mut compared = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
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
                                is_false: c
                                    .get("false")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                            }
                        }),
                    }),
                }
            })
            .collect();
        let request = DecisionRequest {
            operation: "parity".into(),
            pack_id: "parity".into(),
            pack_version: "1".into(),
            state: match &case["state"] {
                serde_json::Value::String(text) => DecisionState::from_text(text.clone()),
                other => DecisionState::from_json(other.clone()),
            },
            questions: questions.clone(),
        };
        let config = model.agent_config().clone();
        let rows =
            magician_decision::adapters::laya::build_rows(&encode, &special, &config, &request)
                .expect("rows");
        let response = model.evaluate(request).await.expect("evaluates");
        for (question, row) in questions.iter().zip(rows) {
            let qid = question.id().as_str();
            let expected = &reference[name][qid];
            let ids: Vec<u32> = expected["ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u32)
                .collect();
            assert_eq!(row.ids, ids, "{name}/{qid}: token ids differ");
            let want: Vec<f64> = expected["probabilities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
            let got: Vec<f64> = match &response.answers[question.id()] {
                Answer::Choice { probabilities, .. } => probabilities.values().copied().collect(),
                Answer::Score { probabilities, .. } => probabilities.clone(),
                Answer::Noul { noul } => {
                    vec![magician_decision::adapters::laya::round4(1.0 - noul), *noul]
                },
            };
            for (g, w) in got.iter().zip(&want) {
                assert!(
                    (g - w).abs() <= 1e-4,
                    "{name}/{qid}: {got:?} vs reference {want:?}"
                );
            }
            if let Answer::Choice { confidence, .. } | Answer::Score { confidence, .. } =
                &response.answers[question.id()]
            {
                let want = expected["confidence"].as_f64().unwrap();
                assert!(
                    (confidence - want).abs() <= 1e-4,
                    "{name}/{qid}: confidence {confidence} vs {want}"
                );
            }
            compared += 1;
        }
    }
    eprintln!("[laya-onnx] parity: {compared} questions match the reference runtime");
    assert!(compared > 0);
}
