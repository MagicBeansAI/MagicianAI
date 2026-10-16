//! Recompose a recorded typed-model run with the current composer:
//!
//! ```text
//! alignment-replay --answers <answers.jsonl> --cases <cases.jsonl> [--cases ...]
//!                  --out <dir> [--thresholds 0.5,0.6] [--bars <bars.json>]
//! ```
//!
//! `--bars` is a JSON object with any of `threshold`, `explicit_benign`,
//! `specified` and `soft` (the `preference_alignment.composer` shape);
//! omitted fields keep their defaults. Writes `answers.jsonl` and one
//! `predictions-tNNN.jsonl` per threshold for the preference-alignment scorer.

use std::path::PathBuf;
use std::process::ExitCode;

use magician_alignment::composer::ComposerBars;
use magician_alignment::eval_case::parse_cases;
use magician_alignment::replay::{read_answers, write_predictions};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("alignment-replay: {error}");
            ExitCode::FAILURE
        },
    }
}

fn run() -> Result<(), String> {
    let mut answers = None;
    let mut cases = Vec::new();
    let mut out = None;
    let mut thresholds = None;
    let mut bars = ComposerBars::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--answers" => answers = Some(PathBuf::from(value()?)),
            "--cases" => cases.push(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--thresholds" => {
                thresholds = Some(
                    value()?
                        .split(',')
                        .map(|t| t.trim().parse::<f64>().map_err(|e| format!("threshold {t}: {e}")))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            },
            "--bars" => {
                let path = value()?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                bars = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
            },
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let answers = answers.ok_or("--answers is required")?;
    let out = out.ok_or("--out is required")?;
    if cases.is_empty() {
        return Err("at least one --cases file is required".to_string());
    }
    bars.validate()?;
    let thresholds = thresholds.unwrap_or_else(|| vec![bars.threshold]);
    let mut parsed = Vec::new();
    for path in &cases {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        parsed.extend(parse_cases(&text).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    let text = std::fs::read_to_string(&answers).map_err(|e| format!("{}: {e}", answers.display()))?;
    let records = read_answers(&text)?;
    for (path, unresolved) in write_predictions(&out, &parsed, records, &bars, &thresholds)
        .map_err(|e| format!("{}: {e}", out.display()))?
    {
        println!("{} ({unresolved} unresolved)", path.display());
    }
    Ok(())
}
