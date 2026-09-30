//! IR round-trips: the wire contract of the plane must survive serde in
//! both directions, because adapters and hosts on opposite sides of the
//! trait both serialize these types (shadow rings, eval snapshots, packs).

use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::{Answer, DecisionRequest, DecisionResponse, DecisionState, Usage};
use magician_decision::ModelIdentity;
use std::collections::BTreeMap;

fn sample_request() -> DecisionRequest {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        OptionId::new("needs_reply"),
        Criteria::Contrastive {
            what: "The sender is waiting on this owner".to_string(),
            not_for: Some("FYI threads with no obligation".to_string()),
            examples: vec!["can you confirm by Friday".to_string()],
        },
    );
    criteria.insert(
        OptionId::new("fyi"),
        Criteria::Str("Information only, no obligation".to_string()),
    );
    DecisionRequest {
        operation: "channel_judge".to_string(),
        pack_id: "channel_judge".to_string(),
        pack_version: "1.0.0".to_string(),
        state: DecisionState::from_json(serde_json::json!({
            "subject": "Re: invoice",
            "latest_summary": "Asks to confirm the March invoice"
        })),
        questions: vec![
            Question::Choice(ChoiceQuestion {
                id: QuestionId::new("lane"),
                instructions: Instruction::Text("Which lane applies".to_string()),
                criteria,
            }),
            Question::Score(ScoreQuestion {
                id: QuestionId::new("urgency"),
                instructions: Instruction::Structured(serde_json::json!({
                    "ask": "How urgent",
                    "scale_note": "0 low, 2 high"
                })),
                levels: vec![
                    Criteria::Str("low".to_string()),
                    Criteria::Str("normal".to_string()),
                    Criteria::Str("high".to_string()),
                ],
            }),
            Question::Noul(NoulQuestion {
                id: QuestionId::new("should_surface"),
                instructions: Instruction::Text("Worth a card at all".to_string()),
                criteria: None,
            }),
        ],
    }
}

#[test]
fn request_roundtrips_through_json() {
    let request = sample_request();
    let json = serde_json::to_value(&request).expect("serialize");
    let back: DecisionRequest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(request, back);
}

#[test]
fn answer_roundtrips_through_json() {
    let mut probabilities = BTreeMap::new();
    probabilities.insert(OptionId::new("needs_reply"), 0.84);
    probabilities.insert(OptionId::new("fyi"), 0.16);
    let answers = vec![
        Answer::Choice {
            choice: OptionId::new("needs_reply"),
            probabilities,
            confidence: 0.596,
        },
        Answer::Score {
            score: 1.035,
            probabilities: vec![0.05, 0.88, 0.07],
            confidence: 0.842,
        },
        Answer::Noul { noul: 0.999 },
    ];
    for answer in answers {
        let json = serde_json::to_value(&answer).expect("serialize");
        let back: Answer = serde_json::from_value(json).expect("deserialize");
        assert_eq!(answer, back);
    }
}

#[test]
fn response_roundtrips_through_json() {
    let response = DecisionResponse {
        model: ModelIdentity::new("typesafe", "jev-1.13.0"),
        pack_id: "channel_judge".to_string(),
        pack_version: "1.0.0".to_string(),
        answers: BTreeMap::from([(
            QuestionId::new("should_surface"),
            Answer::Noul { noul: 0.999 },
        )]),
        usage: Usage {
            input_tokens: 312,
            output_tokens: 48,
        },
    };
    let json = serde_json::to_value(&response).expect("serialize");
    let back: DecisionResponse = serde_json::from_value(json).expect("deserialize");
    assert_eq!(response, back);
}
