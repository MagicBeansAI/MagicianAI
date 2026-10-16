use magician::magician_v2::content_sources::{evaluate_extraction_quality, ReadDepth};
use serde::Serialize;

#[derive(Serialize)]
struct EvalResult {
    name: &'static str,
    depth: &'static str,
    expected_sufficient: bool,
    sufficient: bool,
    char_count: usize,
    word_count: usize,
    score: f64,
}

fn main() {
    let cases = [
        (
            "article-full-text",
            ReadDepth::FullText,
            true,
            "A durable article should contain enough connected prose to support a full-text read. \
             This fixture deliberately uses neutral language because the quality gate measures \
             extraction completeness rather than matching topic-specific phrases. It includes \
             multiple sentences and enough detail to clear the configured structural threshold.",
        ),
        (
            "concise-gist",
            ReadDepth::Gist,
            true,
            "A concise extracted paragraph can be sufficient when the caller requested only a gist.",
        ),
        (
            "empty-shell",
            ReadDepth::Gist,
            false,
            "Home Menu Sign in",
        ),
        (
            "thin-full-text",
            ReadDepth::FullText,
            false,
            "A short summary is not a full article.",
        ),
    ];

    let mut failed = false;
    let results = cases
        .into_iter()
        .map(|(name, depth, expected_sufficient, text)| {
            let quality = evaluate_extraction_quality(text, depth, 60, 200);
            failed |= quality.sufficient != expected_sufficient;
            EvalResult {
                name,
                depth: match depth {
                    ReadDepth::Gist => "gist",
                    ReadDepth::FullText => "full_text",
                },
                expected_sufficient,
                sufficient: quality.sufficient,
                char_count: quality.char_count,
                word_count: quality.word_count,
                score: quality.score,
            }
        })
        .collect::<Vec<_>>();

    println!(
        "{}",
        serde_json::to_string_pretty(&results).expect("serializing content reader eval")
    );
    if failed {
        std::process::exit(1);
    }
}
