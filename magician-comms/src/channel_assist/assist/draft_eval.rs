//! Deterministic draft-usefulness evaluation fixtures.
//!
//! This scores a generated/edited draft against action-critical phrases,
//! forbidden hallucinations/secrets, length, and exact writing-preference
//! statements. Fixtures are synthetic and safe to commit.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct DraftEvalFixture {
    pub id: String,
    pub draft: String,
    #[serde(default)]
    pub required_phrases: Vec<String>,
    #[serde(default)]
    pub forbidden_phrases: Vec<String>,
    #[serde(default)]
    pub max_words: Option<usize>,
    #[serde(default)]
    pub writing_preferences: Vec<String>,
    #[serde(default = "default_expected_useful")]
    pub expected_useful: bool,
}

fn default_expected_useful() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DraftEvalResult {
    pub id: String,
    pub useful: bool,
    pub expected_useful: bool,
    pub matched_expectation: bool,
    pub word_count: usize,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DraftEvalReport {
    pub total: usize,
    pub useful: usize,
    pub matched_expectation: usize,
    pub usefulness_rate: f64,
    pub expectation_accuracy: f64,
    pub results: Vec<DraftEvalResult>,
}

pub fn score_fixture(fixture: &DraftEvalFixture) -> DraftEvalResult {
    let normalized = fixture.draft.to_ascii_lowercase();
    let mut failures = Vec::new();
    for phrase in &fixture.required_phrases {
        if !normalized.contains(&phrase.to_ascii_lowercase()) {
            failures.push(format!("missing required phrase: {phrase}"));
        }
    }
    for phrase in &fixture.forbidden_phrases {
        if normalized.contains(&phrase.to_ascii_lowercase()) {
            failures.push(format!("contains forbidden phrase: {phrase}"));
        }
    }
    let word_count = fixture.draft.split_whitespace().count();
    if fixture.max_words.is_some_and(|limit| word_count > limit) {
        failures.push(format!(
            "word count {word_count} exceeds limit {}",
            fixture.max_words.unwrap_or_default()
        ));
    }
    for preference in &fixture.writing_preferences {
        if let Some(failure) = preference_failure(preference, &fixture.draft, word_count) {
            failures.push(failure);
        }
    }
    let useful = failures.is_empty();
    DraftEvalResult {
        id: fixture.id.clone(),
        useful,
        expected_useful: fixture.expected_useful,
        matched_expectation: useful == fixture.expected_useful,
        word_count,
        failures,
    }
}

pub fn score(fixtures: &[DraftEvalFixture]) -> DraftEvalReport {
    let results = fixtures.iter().map(score_fixture).collect::<Vec<_>>();
    let total = results.len();
    let useful = results.iter().filter(|result| result.useful).count();
    let matched_expectation = results
        .iter()
        .filter(|result| result.matched_expectation)
        .count();
    DraftEvalReport {
        total,
        useful,
        matched_expectation,
        usefulness_rate: ratio(useful, total),
        expectation_accuracy: ratio(matched_expectation, total),
        results,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn preference_failure(preference: &str, draft: &str, word_count: usize) -> Option<String> {
    let preference_lower = preference.to_ascii_lowercase();
    if preference_lower.contains("avoid exclamation") && draft.contains('!') {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("keep replies concise") && word_count > 120 {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("include a little more context") && word_count < 20 {
        return Some(format!("violates writing preference: {preference}"));
    }

    let greeting = draft_greeting_style(draft);
    if preference_lower.contains("friendly greeting") && greeting != Some("friendly") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("formal greeting") && greeting != Some("formal") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("casual greeting") && greeting != Some("casual") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("without a greeting") && greeting.is_some() {
        return Some(format!("violates writing preference: {preference}"));
    }

    let signoff = draft_signoff_style(draft);
    if preference_lower.contains("formal sign-off") && signoff != Some("formal") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("friendly sign-off") && signoff != Some("friendly") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("casual sign-off") && signoff != Some("casual") {
        return Some(format!("violates writing preference: {preference}"));
    }
    if preference_lower.contains("without a sign-off") && signoff.is_some() {
        return Some(format!("violates writing preference: {preference}"));
    }

    let supported = [
        "avoid exclamation",
        "keep replies concise",
        "include a little more context",
        "friendly greeting",
        "formal greeting",
        "casual greeting",
        "without a greeting",
        "formal sign-off",
        "friendly sign-off",
        "casual sign-off",
        "without a sign-off",
    ]
    .iter()
    .any(|needle| preference_lower.contains(needle));
    (!supported).then(|| format!("unsupported writing preference: {preference}"))
}

fn draft_greeting_style(draft: &str) -> Option<&'static str> {
    let first = draft
        .trim_start()
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_matches(|character: char| !character.is_alphabetic())
        .to_ascii_lowercase();
    match first.as_str() {
        "dear" => Some("formal"),
        "hi" | "hello" => Some("friendly"),
        "hey" => Some("casual"),
        _ => None,
    }
}

fn draft_signoff_style(draft: &str) -> Option<&'static str> {
    let tail = draft
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if tail.contains("kind regards") || tail.contains("sincerely") {
        Some("formal")
    } else if tail.contains("best,") || tail.contains("thanks,") || tail.contains("thank you,") {
        Some("friendly")
    } else if tail.contains("cheers,") {
        Some("casual")
    } else {
        None
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn detects_useful_and_hallucinated_drafts() {
        let useful = DraftEvalFixture {
            id: "useful".to_string(),
            draft: "Hi Priya,\n\nTuesday at 2 PM works for me.\n\nThanks,".to_string(),
            required_phrases: vec!["Tuesday at 2 PM".to_string()],
            forbidden_phrases: vec!["Wednesday".to_string()],
            max_words: Some(40),
            writing_preferences: vec!["Use a friendly greeting.".to_string()],
            expected_useful: true,
        };
        assert!(score_fixture(&useful).useful);

        let mut bad = useful.clone();
        bad.id = "bad".to_string();
        bad.draft = "Wednesday is confirmed!".to_string();
        bad.writing_preferences = vec!["Avoid exclamation marks.".to_string()];
        bad.expected_useful = false;
        let result = score_fixture(&bad);
        assert!(!result.useful);
        assert!(result.matched_expectation);
        assert!(result.failures.len() >= 2);
    }

    #[test]
    fn checks_every_edit_derived_writing_preference_shape() {
        let compliant = DraftEvalFixture {
            id: "styles".to_string(),
            draft: "Hey Sam,\n\nHere is enough useful context to explain the decision, the timing, and the next step clearly without making the reply unnecessarily long.\n\nCheers,\nAlex"
                .to_string(),
            required_phrases: Vec::new(),
            forbidden_phrases: Vec::new(),
            max_words: Some(80),
            writing_preferences: vec![
                "Use a casual greeting.".to_string(),
                "End replies with a casual sign-off.".to_string(),
                "Include a little more context in replies.".to_string(),
                "Avoid exclamation marks.".to_string(),
            ],
            expected_useful: true,
        };
        assert!(score_fixture(&compliant).useful);

        let unsupported = DraftEvalFixture {
            id: "unsupported".to_string(),
            writing_preferences: vec!["Write like an astronaut.".to_string()],
            ..compliant
        };
        assert_eq!(
            score_fixture(&unsupported).failures,
            vec!["unsupported writing preference: Write like an astronaut."]
        );
    }

    #[test]
    fn committed_draft_fixtures_parse_and_cover_positive_and_negative_cases() {
        let raw =
            include_str!("../../../../magician/tests/fixtures/channel_draft_usefulness_eval.jsonl");
        let fixtures = raw
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<DraftEvalFixture>)
            .collect::<Result<Vec<_>, _>>()
            .expect("valid draft fixture JSONL");
        assert!(fixtures.len() >= 12);
        assert!(fixtures.iter().any(|fixture| fixture.expected_useful));
        assert!(fixtures.iter().any(|fixture| !fixture.expected_useful));
        let report = score(&fixtures);
        assert_eq!(report.expectation_accuracy, 1.0, "{:#?}", report.results);
    }
}
