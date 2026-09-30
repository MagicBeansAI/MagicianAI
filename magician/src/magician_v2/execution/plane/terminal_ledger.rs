//! The runless caller's ledger (plane Task 12b).
//!
//! A T3 terminal has no run, so it has no execution record, no cost ledger,
//! and no episode. The ledger is its answer to "what did this session
//! actually do": every governed `tools/call` outcome, keyed by MCP session —
//! never by grant, because two terminals on one grant must not collide
//! (Task 12c) and their attribution must stay separate.
//!
//! Deliberately NOT an episode and NOT memory: an audit log is not memory
//! (the plan's own line). A caller that wants its work remembered well
//! starts a task — T2 writes the ordinary run-keyed episode and needs
//! nothing here.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;

use once_cell::sync::Lazy;
use serde_json::{json, Value};

/// One governed call's outcome, as the runless caller's audit trail.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalLedgerEntry {
    pub tool: String,
    /// Epoch milliseconds.
    pub timestamp_ms: i64,
    pub outcome: TerminalLedgerOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TerminalLedgerOutcome {
    /// The governed dispatch ran; carries the MCP `isError` flag of its
    /// result so tool-level failures are visible without storing content.
    Executed { is_error: bool },
    /// The call never dispatched. Carries the machine-readable refusal
    /// reason (`not_available`, `needs_approval`, `unwired`, `revoked`,
    /// `turn_budget_spent`, `elicitation_pending`, `no_mouth_bridge`).
    Refused { reason: String },
    /// The call parked for an elicitation answer (Task 12a).
    ElicitationRequested { request_id: String },
}

/// Bounded per session: an unbounded audit trail on a long-lived terminal
/// is a slow leak. Oldest entries drop first when the cap is hit.
const MAX_ENTRIES_PER_SESSION: usize = 512;

static LEDGER: Lazy<StdMutex<HashMap<String, Vec<TerminalLedgerEntry>>>> =
    Lazy::new(|| StdMutex::new(HashMap::new()));

/// Record one outcome against an MCP session id (the server-minted
/// `pltsess_` value — never client-supplied).
pub fn record(session_id: &str, entry: TerminalLedgerEntry) {
    let mut ledger = LEDGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entries = ledger.entry(session_id.to_string()).or_default();
    entries.push(entry);
    if entries.len() > MAX_ENTRIES_PER_SESSION {
        let overflow = entries.len() - MAX_ENTRIES_PER_SESSION;
        entries.drain(..overflow);
    }
}

/// Read (without clearing) a session's ledger, oldest first.
pub fn entries(session_id: &str) -> Vec<TerminalLedgerEntry> {
    LEDGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// Take a session's ledger, clearing it — the close-of-session read.
pub fn take(session_id: &str) -> Vec<TerminalLedgerEntry> {
    LEDGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id)
        .unwrap_or_default()
}

/// MCP `tools/call` shape for the `session_ledger` hot tool: the caller's
/// own audit trail. Content only — no credentials, no arguments, no results.
pub fn ledger_as_mcp_result(session_id: &str) -> Value {
    let entries = entries(session_id);
    json!({
        "isError": false,
        "content": [{
            "type": "text",
            "text": format!("{} recorded call(s) for session {session_id}", entries.len()),
        }],
        "session_id": session_id,
        "entries": entries
            .iter()
            .map(|entry| {
                let (outcome, detail) = match &entry.outcome {
                    TerminalLedgerOutcome::Executed { is_error } => {
                        ("executed".to_string(), json!({ "isError": is_error }))
                    },
                    TerminalLedgerOutcome::Refused { reason } => {
                        ("refused".to_string(), json!({ "reason": reason }))
                    },
                    TerminalLedgerOutcome::ElicitationRequested { request_id } => (
                        "elicitation".to_string(),
                        json!({ "requestId": request_id }),
                    ),
                };
                json!({
                    "tool": entry.tool,
                    "timestampMs": entry.timestamp_ms,
                    "outcome": outcome,
                    "detail": detail,
                })
            })
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executed(tool: &str) -> TerminalLedgerEntry {
        TerminalLedgerEntry {
            tool: tool.to_string(),
            timestamp_ms: 1,
            outcome: TerminalLedgerOutcome::Executed { is_error: false },
        }
    }

    /// Task 12c's core check, pinned: two terminals on the SAME grant key
    /// their ledgers by session, so attribution never collides.
    #[test]
    fn two_sessions_on_one_grant_never_share_ledger_entries() {
        let a = "pltsess_a";
        let b = "pltsess_b";
        record(a, executed("read_file"));
        record(b, executed("search_memory"));
        let a_entries = entries(a);
        let b_entries = entries(b);
        assert_eq!(a_entries.len(), 1);
        assert_eq!(a_entries[0].tool, "read_file");
        assert_eq!(b_entries.len(), 1);
        assert_eq!(b_entries[0].tool, "search_memory");
        take(a);
        take(b);
    }

    #[test]
    fn the_ledger_is_bounded_per_session() {
        let session = "pltsess_capped";
        for index in 0..(MAX_ENTRIES_PER_SESSION + 64) {
            record(
                session,
                TerminalLedgerEntry {
                    tool: format!("tool_{index}"),
                    timestamp_ms: index as i64,
                    outcome: TerminalLedgerOutcome::Executed { is_error: false },
                },
            );
        }
        let bounded = entries(session);
        assert_eq!(bounded.len(), MAX_ENTRIES_PER_SESSION);
        // Oldest dropped first: the first surviving entry is past the
        // overflow, not the first recorded.
        assert_eq!(
            bounded.first().map(|entry| entry.timestamp_ms),
            Some(64),
            "the oldest entries must drop first"
        );
        take(session);
    }

    #[test]
    fn take_clears_the_session() {
        let session = "pltsess_take";
        record(session, executed("read_file"));
        assert_eq!(take(session).len(), 1);
        assert!(entries(session).is_empty());
    }
}
