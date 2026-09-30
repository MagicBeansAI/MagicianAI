//! Price saved Decision Model receipts with the ledger's dated builtin registry.
//! JSON receipts on stdin; content-free pricing facts on stdout. No model calls.
use std::collections::HashSet;
use std::io::{self, Read};

use decision_engine_contract::telemetry::DecisionModelCall;
use magician::magician_v2::analytics::decision_model_telemetry::{call_usage, pricing};

fn main() -> anyhow::Result<()> {
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    let mut input = String::new();
    io::stdin().take(MAX_BYTES + 1).read_to_string(&mut input)?;
    anyhow::ensure!(
        input.len() as u64 <= MAX_BYTES,
        "receipt input exceeds 8 MiB"
    );
    let calls: Vec<DecisionModelCall> = serde_json::from_str(&input)?;
    let mut seen = HashSet::new();
    anyhow::ensure!(
        calls.iter().all(|call| seen.insert(&call.call_id)),
        "duplicate physical call receipt"
    );
    let facts: Vec<_> = calls
        .iter()
        .map(|call| {
            let fact = pricing(
                &call.provider,
                &call.model,
                call.started_at_ms,
                &call_usage(call),
            );
            serde_json::json!({"call_id": call.call_id, "pricing": fact})
        })
        .collect();
    serde_json::to_writer(io::stdout().lock(), &facts)?;
    Ok(())
}
