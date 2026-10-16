//! Recompose recorded typed-model answers into scored predictions, with no
//! model calls: the same composer at any bars, over the same answers, so a
//! composer change is compared on identical inputs.
//!
//! `answers.jsonl` is what the live engine lane records: one line per case
//! with `id` and `items[{item_id, heads}]`. Predictions are
//! `alignment_decision.v2` records the preference-alignment scorer reads
//! (`scripts/eval-preference-alignment-judgment.py score`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::composer::{compose, ComposerBars, IntentionHeads};
use crate::eval_case::EvalCase;

/// One case's record: its id, the raw answer line, and its per-intention
/// answers when every item answered.
pub type Record = (String, Value, Option<BTreeMap<String, IntentionHeads>>);

/// Parse a recorded `answers.jsonl`. A case with any item missing its
/// answers gets `None` and composes as unresolved.
pub fn read_answers(text: &str) -> Result<Vec<Record>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let raw: Value =
                serde_json::from_str(line).map_err(|error| format!("answer record: {error}"))?;
            let id = raw["id"].as_str().unwrap_or_default().to_string();
            let heads = raw["items"].as_array().and_then(|items| {
                items
                    .iter()
                    .map(|item| {
                        let heads: IntentionHeads =
                            serde_json::from_value(item["heads"].clone()).ok()?;
                        Some((item["item_id"].as_str()?.to_string(), heads))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()
            });
            Ok((id, raw, heads))
        })
        .collect()
}

/// Write `answers.jsonl` once, then compose and write
/// `predictions-tNNN.jsonl` at each threshold over `bars`. Records for cases
/// not in `cases` are dropped. Returns each predictions file with its
/// unresolved count.
pub fn write_predictions(
    out_dir: &Path,
    cases: &[EvalCase],
    mut records: Vec<Record>,
    bars: &ComposerBars,
    thresholds: &[f64],
) -> std::io::Result<Vec<(PathBuf, usize)>> {
    let by_id: BTreeMap<&str, &EvalCase> =
        cases.iter().map(|case| (case.id.as_str(), case)).collect();
    records.retain(|(id, _, _)| by_id.contains_key(id.as_str()));
    records.sort_by(|a, b| a.0.cmp(&b.0));
    std::fs::create_dir_all(out_dir)?;
    let raw_body: String = records.iter().map(|(_, raw, _)| format!("{raw}\n")).collect();
    std::fs::write(out_dir.join("answers.jsonl"), raw_body)?;
    let mut written = Vec::new();
    for threshold in thresholds {
        let bars = bars.at_threshold(*threshold);
        let mut unresolved = 0;
        let body: String = records
            .iter()
            .map(|(id, raw, heads)| {
                let input = by_id[id.as_str()].to_alignment_input();
                let composed = heads
                    .as_ref()
                    .ok_or_else(|| "engine_answer_missing".to_string())
                    .and_then(|heads| compose(&input, heads, &bars).map_err(|error| error.to_string()));
                let mut record = json!({
                    "id": id,
                    "latency_ms": raw["latency_ms"],
                    "usage": raw["usage"],
                });
                match composed {
                    Ok(decision) => record["decision"] = json!(decision),
                    Err(cause) => {
                        unresolved += 1;
                        record["decision"] = Value::Null;
                        record["unresolved"] = json!(cause);
                    },
                }
                format!("{record}\n")
            })
            .collect();
        let path = out_dir.join(format!(
            "predictions-t{:03}.jsonl",
            (threshold * 100.0).round() as u32
        ));
        std::fs::write(&path, body)?;
        written.push((path, unresolved));
    }
    Ok(written)
}
