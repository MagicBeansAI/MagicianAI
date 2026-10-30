use super::{candidates::prepare_candidate_set, validation::validate_request};
use decision_engine_contract::action::{ActionRequest, ToolCall};
use serde::Deserialize;
use serde_json::json;
use std::io::BufRead;

#[derive(Deserialize)]
struct Case {
    request: ActionRequest,
    expected_call: ToolCall,
    #[serde(default)]
    ignore_arguments: Vec<String>,
    #[serde(default)]
    baseline_offered: bool,
    #[serde(default)]
    baseline_compatible: bool,
}

#[test]
#[ignore = "requires an explicitly supplied private JSONL replay fixture; no providers"]
fn retained_candidate_replay() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("MAGICIAN_ACTION_REPLAY")?;
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let (mut total, mut before, mut after, mut gained, mut lost, mut empty, mut truncated) =
        (0, 0, 0, 0, 0, 0, 0);
    let (mut compatible_before, mut compatible_after) = (0, 0);
    let mut rows = Vec::new();
    let mut timings = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut case: Case = serde_json::from_str(&line)?;
        validate_request(&case.request).map_err(|reason| format!("case {index}: {reason}"))?;
        let started = std::time::Instant::now();
        let result = prepare_candidate_set(&case.request);
        let micros = started.elapsed().as_micros() as u64;
        timings.push(micros);
        for key in &case.ignore_arguments {
            case.expected_call
                .arguments
                .as_object_mut()
                .unwrap()
                .remove(key);
        }
        let calls: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| {
                let mut call = candidate.call.clone();
                for key in &case.ignore_arguments {
                    call.arguments.as_object_mut().unwrap().remove(key);
                }
                call
            })
            .collect();
        let offered = calls.contains(&case.expected_call);
        // A valid smaller call can use defaults for omitted optional fields.
        // This is coverage, not proof of equivalence or task usefulness.
        let compatible = calls.iter().any(|call| {
            call.tool == case.expected_call.tool
                && (!call.arguments.as_object().unwrap().is_empty()
                    || case.expected_call.arguments.as_object().unwrap().is_empty())
                && call
                    .arguments
                    .as_object()
                    .unwrap()
                    .iter()
                    .all(|(key, value)| case.expected_call.arguments.get(key) == Some(value))
        });
        compatible_before += usize::from(case.baseline_compatible);
        compatible_after += usize::from(compatible);
        total += 1;
        before += usize::from(case.baseline_offered);
        after += usize::from(offered);
        gained += usize::from(offered && !case.baseline_offered);
        lost += usize::from(!offered && case.baseline_offered);
        empty += usize::from(result.candidates.is_empty());
        truncated += usize::from(result.truncated);
        rows.push(
            json!({"case":index,"before":case.baseline_offered,"after":offered,
            "compatible_before":case.baseline_compatible,"compatible_after":compatible,
            "candidates":result.candidates.len(),"construction_us":micros}),
        );
    }
    timings.sort_unstable();
    let quantile = |p: usize| {
        timings
            .get(timings.len().saturating_sub(1) * p / 100)
            .copied()
    };
    assert!(total > 0, "empty replay fixture");
    let report = serde_json::to_string_pretty(&json!({
        "compatible_before":compatible_before,"compatible_after":compatible_after,
        "total":total,"baseline_offered":before,"offered":after,"gained":gained,"lost":lost,
        "no_candidates":empty,"truncated":truncated,"construction_us_p50":quantile(50),
        "construction_us_p95":quantile(95),"construction_us_max":timings.last(),"cases":rows
    }))?;
    if let Ok(path) = std::env::var("MAGICIAN_ACTION_REPLAY_OUTPUT") {
        std::fs::write(path, &report)?;
    }
    println!("{report}");
    Ok(())
}
