//! Provider-free tier-effectiveness eval over a memory temperature overlay.
//!
//! Reads an overlay **read-only** and reports whether the temperature tiering
//! is doing its job: is the active tier earned, is the working set bounded,
//! does the tier predict use, is the overlay accumulating dead weight, and are
//! candidate keys well-formed.
//!
//! No provider, no network, no index — safe to point at live scope data.
//!
//! ```text
//! cargo run -p magician-vector-index --example memory_tier_health -- \
//!     --overlay <scope>/memory/index/temperature_overlay.json
//! ```

use std::process::ExitCode;

use chrono::Utc;
use magician_vector_index::memory_candidates::MemoryCandidateDocument;
use magician_vector_index::memory_temperature::memory_temperature_candidate_key;
use magician_vector_index::memory_temperature::{
    apply_memory_temperature_maintenance, MemoryTemperatureOverlay,
};
use magician_vector_index::memory_tier_health::{
    compute_memory_tier_health_with_live_keys, evaluate_memory_tier_health, MemoryTierHealthGates,
};

struct Args {
    overlay: String,
    documents: Option<String>,
    json_only: bool,
    simulate_maintenance: bool,
    gates: MemoryTierHealthGates,
}

fn parse_args() -> Result<Args, String> {
    let mut overlay = None;
    let mut documents = None;
    let mut json_only = false;
    let mut simulate_maintenance = false;
    let mut gates = MemoryTierHealthGates::default();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("`{arg}` requires a value"))
        };
        match arg.as_str() {
            "--overlay" => overlay = Some(value()?),
            "--documents" => documents = Some(value()?),
            "--json" => json_only = true,
            "--simulate-maintenance" => simulate_maintenance = true,
            "--max-unearned-active-ratio" => {
                gates.max_unearned_active_ratio = value()?.parse().map_err(|_| "expected f64")?
            },
            "--max-working-set-ratio" => {
                gates.max_working_set_ratio = value()?.parse().map_err(|_| "expected f64")?
            },
            "--min-tier-lift" => {
                gates.min_tier_lift = value()?.parse().map_err(|_| "expected f64")?
            },
            "--max-dead-entry-ratio" => {
                gates.max_dead_entry_ratio = value()?.parse().map_err(|_| "expected f64")?
            },
            "--max-unmigrated-live-ratio" => {
                gates.max_unmigrated_live_ratio = value()?.parse().map_err(|_| "expected f64")?
            },
            "--max-key-chars" => {
                gates.max_key_chars = value()?.parse().map_err(|_| "expected usize")?
            },
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            },
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    Ok(Args {
        overlay: overlay.ok_or_else(|| format!("--overlay is required\n\n{}", usage()))?,
        documents,
        json_only,
        simulate_maintenance,
        gates,
    })
}

fn usage() -> String {
    [
        "usage: memory_tier_health --overlay <temperature_overlay.json> [options]",
        "",
        "  --documents <documents.jsonl>   the scope's indexed candidates, so the",
        "                                  migration gate can tell memory that can",
        "                                  still migrate from evidence kept under",
        "                                  its historical key",
        "  --json                          emit only the JSON report",
        "  --simulate-maintenance          also report metrics after an in-memory",
        "                                  maintenance pass (nothing is written)",
        "  --max-unearned-active-ratio F   default 0.05",
        "  --max-working-set-ratio F       default 0.35",
        "  --min-tier-lift F               default 1.5",
        "  --max-dead-entry-ratio F        default 0.50",
        "  --max-unmigrated-live-ratio F   default 0.0 (needs --documents)",
        "  --max-key-chars N               default 1024",
    ]
    .join("\n")
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        },
    };

    let raw = match std::fs::read(&args.overlay) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("failed to read {}: {error}", args.overlay);
            return ExitCode::from(2);
        },
    };
    let overlay: MemoryTemperatureOverlay = match serde_json::from_slice(&raw) {
        Ok(overlay) => overlay,
        Err(error) => {
            eprintln!("failed to parse {}: {error}", args.overlay);
            return ExitCode::from(2);
        },
    };

    // Without this the migration gate abstains: a legacy key on an entry whose
    // candidate is gone is evidence, not debt, and the two are indistinguishable
    // from the overlay alone.
    let live_keys = match args.documents.as_ref() {
        Some(path) => match read_live_candidate_keys(path) {
            Ok(keys) => Some(keys),
            Err(error) => {
                eprintln!("failed to read {path}: {error}");
                return ExitCode::from(2);
            },
        },
        None => None,
    };
    let metrics = compute_memory_tier_health_with_live_keys(&overlay, live_keys.as_ref());
    // Run the current maintenance rules over a private copy so the effect of a
    // tiering change can be measured against real data without writing to it.
    let simulated = args.simulate_maintenance.then(|| {
        let mut projected = overlay.clone();
        let summary = apply_memory_temperature_maintenance(&mut projected, Utc::now());
        (
            compute_memory_tier_health_with_live_keys(&projected, live_keys.as_ref()),
            summary,
        )
    });
    let gate_results = evaluate_memory_tier_health(&metrics, args.gates);
    let failed = gate_results
        .iter()
        .filter(|gate| !gate.passed && !gate.skipped)
        .count();

    let report = serde_json::json!({
        "overlay_path": args.overlay,
        "overlay_bytes": raw.len(),
        "schema_version": overlay.schema_version,
        "metrics": metrics,
        "gates": gate_results,
        "failed_gates": failed,
        "simulated_after_maintenance": simulated.as_ref().map(|(metrics, summary)| {
            serde_json::json!({
                "metrics": metrics,
                "reviewed": summary.reviewed,
                "changed": summary.changed,
                "promoted": summary.promoted,
                "demoted": summary.demoted,
                "gates": evaluate_memory_tier_health(metrics, args.gates),
            })
        }),
    });

    if args.json_only {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return exit_code(failed);
    }

    println!("memory tier health — {}", args.overlay);
    println!(
        "  overlay              {} bytes, schema v{}",
        raw.len(),
        overlay.schema_version
    );
    println!(
        "  entries              {} (T0 {} / T1 {} / T2 {} / T3 {})",
        metrics.total_entries, metrics.t0, metrics.t1, metrics.t2, metrics.t3
    );
    println!(
        "  working set          {:.1}% of overlay",
        metrics.working_set_ratio * 100.0
    );
    println!(
        "  unearned active      {} of {} ({:.1}%)",
        metrics.unearned_active_count,
        metrics.t0 + metrics.t1,
        metrics.unearned_active_ratio * 100.0
    );
    match metrics.tier_lift {
        Some(lift) => println!(
            "  tier lift            {lift:.2}x  (active {:.1}% selected vs cold {:.1}%)",
            metrics.active_selected_rate * 100.0,
            metrics.cold_selected_rate * 100.0
        ),
        None => println!("  tier lift            n/a (one side empty)"),
    }
    println!(
        "  dead entries         {} ({:.1}%)",
        metrics.dead_entry_count,
        metrics.dead_entry_ratio * 100.0
    );
    println!(
        "  legacy keys          {} ({:.1}%), longest key {} chars",
        metrics.legacy_key_count,
        metrics.legacy_key_ratio * 100.0,
        metrics.max_key_chars
    );
    match metrics.unmigrated_live_count {
        Some(count) => println!(
            "  unmigrated live      {count} ({:.1}% of live memory)",
            metrics.unmigrated_live_ratio.unwrap_or(0.0) * 100.0
        ),
        None => println!("  unmigrated live      n/a (pass --documents to measure)"),
    }
    println!(
        "  partitions           {} ({:.1}% of entries alone in theirs)",
        metrics.partition_count,
        metrics.singleton_partition_ratio * 100.0
    );
    println!(
        "  superseded           {} ({} left outside T3)",
        metrics.superseded_count, metrics.superseded_active_count
    );

    println!("\n  lanes:");
    for (lane, health) in &metrics.lanes {
        println!(
            "    {lane:<18} {:>5} entries  {:>4} active  {:>4} unearned",
            health.entries, health.active, health.unearned_active
        );
    }

    println!("\n  gates:");
    for gate in &gate_results {
        let status = if gate.skipped {
            "SKIP"
        } else if gate.passed {
            "PASS"
        } else {
            "FAIL"
        };
        let comparator = if gate.upper_bound { "<=" } else { ">=" };
        println!(
            "    [{status}] {:<32} {:.4} {comparator} {:.4}",
            gate.name, gate.actual, gate.threshold
        );
    }

    if let Some((after, summary)) = simulated.as_ref() {
        println!("\n  simulated after one maintenance pass (nothing written):");
        println!(
            "    reviewed {} / changed {} / promoted {} / demoted {}",
            summary.reviewed, summary.changed, summary.promoted, summary.demoted
        );
        println!(
            "    entries              T0 {} / T1 {} / T2 {} / T3 {}",
            after.t0, after.t1, after.t2, after.t3
        );
        println!(
            "    working set          {:.1}%  (was {:.1}%)",
            after.working_set_ratio * 100.0,
            metrics.working_set_ratio * 100.0
        );
        println!(
            "    unearned active      {} ({:.1}%)  (was {} / {:.1}%)",
            after.unearned_active_count,
            after.unearned_active_ratio * 100.0,
            metrics.unearned_active_count,
            metrics.unearned_active_ratio * 100.0
        );
        match (after.tier_lift, metrics.tier_lift) {
            (Some(after_lift), Some(before_lift)) => {
                println!("    tier lift            {after_lift:.2}x  (was {before_lift:.2}x)")
            },
            (Some(after_lift), None) => println!("    tier lift            {after_lift:.2}x"),
            _ => {},
        }
    }

    if failed > 0 {
        println!("\n{failed} gate(s) failed");
    } else {
        println!("\nall gates passed");
    }
    exit_code(failed)
}

/// Rebuild the current candidate keys from the scope's indexed documents.
///
/// `documents.jsonl` is a serialized `MemoryCandidateDocument` per line, so the
/// keys come from the same function the runtime writes with rather than a
/// re-implementation that could drift.
fn read_live_candidate_keys(path: &str) -> std::io::Result<std::collections::BTreeSet<String>> {
    let raw = std::fs::read_to_string(path)?;
    let mut keys = std::collections::BTreeSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let candidate: MemoryCandidateDocument = match serde_json::from_str(line) {
            Ok(candidate) => candidate,
            Err(_) => continue,
        };
        keys.insert(memory_temperature_candidate_key(&candidate));
        keys.insert(legacy_candidate_key(&candidate));
    }
    Ok(keys)
}

/// The pre-v6 key for the same candidate, so an un-migrated overlay is still
/// recognised as live rather than counted as evidence.
fn legacy_candidate_key(candidate: &MemoryCandidateDocument) -> String {
    let scope = match candidate.scope {
        magician_vector_index::memory_tiers::TierScope::User => "user",
        magician_vector_index::memory_tiers::TierScope::Agent => "agent",
        magician_vector_index::memory_tiers::TierScope::AgentGoal => "agent_goal",
    };
    format!(
        "{scope}:{}:{}:{}:{}",
        candidate.agent_id.as_deref().unwrap_or(""),
        candidate.goal_id.as_deref().unwrap_or(""),
        candidate.tier_name,
        candidate.item_key
    )
}

fn exit_code(failed: usize) -> ExitCode {
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
