//! Crash recovery: scan journal for unmatched reserve entries (no matching commit/rollback).
//! Flag stale reservations for reconciliation. Load from persisted state.

use std::collections::HashSet;

use super::ledger::{Reservation, ResourceLedger};
use super::types::ReservationId;

/// A reservation found in the journal that has no matching commit or rollback.
#[derive(Debug, Clone)]
pub struct UnmatchedReservation {
    pub reservation_id: ReservationId,
    pub token_id: String,
    pub commodity: String,
    pub amount: rust_decimal::Decimal,
    pub agent_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Scan the journal for reserve entries that lack a matching commit or rollback.
/// Returns the set of unmatched reservation IDs that need reconciliation.
///
/// Algorithm:
/// 1. Walk the journal chronologically.
/// 2. Reserve references contain "reserve:" in their reference field.
/// 3. Commit references contain "commit:" and rollback references contain "rollback:".
/// 4. Any reservation that was reserved but never committed or rolled back is unmatched.
pub fn find_unmatched_reservations(ledger: &ResourceLedger) -> Vec<UnmatchedReservation> {
    let mut reserved: HashSet<String> = HashSet::new();
    let mut committed_or_rolled_back: HashSet<String> = HashSet::new();

    // Collect reservation IDs from journal references
    for entry in &ledger.journal {
        if entry.reference.starts_with("reserve:") {
            let rid = entry
                .reference
                .strip_prefix("reserve:")
                .unwrap_or("")
                .to_string();
            reserved.insert(rid);
        } else if entry.reference.starts_with("commit:") {
            let rid = entry
                .reference
                .strip_prefix("commit:")
                .unwrap_or("")
                .to_string();
            committed_or_rolled_back.insert(rid);
        } else if entry.reference.starts_with("commit_with_amount:") {
            let rid = entry
                .reference
                .strip_prefix("commit_with_amount:")
                .unwrap_or("")
                .to_string();
            committed_or_rolled_back.insert(rid);
        } else if entry.reference.starts_with("rollback:") {
            let rid = entry
                .reference
                .strip_prefix("rollback:")
                .unwrap_or("")
                .to_string();
            committed_or_rolled_back.insert(rid);
        } else if entry.reference.starts_with("return_delta:") {
            let rid = entry
                .reference
                .strip_prefix("return_delta:")
                .unwrap_or("")
                .to_string();
            committed_or_rolled_back.insert(rid);
        } else if entry.reference.starts_with("overage:") {
            let rid = entry
                .reference
                .strip_prefix("overage:")
                .unwrap_or("")
                .to_string();
            committed_or_rolled_back.insert(rid);
        }
    }

    // Find unmatched: reserved but not committed/rolled back
    let unmatched_ids: Vec<String> = reserved
        .difference(&committed_or_rolled_back)
        .cloned()
        .collect();

    // Build UnmatchedReservation from active_reservations or from journal metadata
    let mut result = Vec::new();
    for rid in unmatched_ids {
        let reservation_id = ReservationId(rid.clone());
        if let Some(reservation) = ledger.active_reservations.get(&reservation_id) {
            result.push(UnmatchedReservation {
                reservation_id: reservation.id.clone(),
                token_id: reservation.token_id.clone(),
                commodity: reservation.commodity.clone(),
                amount: reservation.amount,
                agent_id: reservation.agent_id.clone(),
                created_at: reservation.created_at,
            });
        } else {
            // Reservation info might be in journal metadata
            for entry in &ledger.journal {
                if entry.reference == format!("reserve:{}", rid) {
                    let token_id = entry.metadata.get("token_id").cloned().unwrap_or_default();
                    let commodity = entry.metadata.get("commodity").cloned().unwrap_or_default();
                    let amount_str = entry.metadata.get("amount").cloned().unwrap_or_default();
                    let amount = amount_str
                        .parse::<rust_decimal::Decimal>()
                        .unwrap_or_default();
                    result.push(UnmatchedReservation {
                        reservation_id: reservation_id.clone(),
                        token_id,
                        commodity,
                        amount,
                        agent_id: entry.agent_id.clone(),
                        created_at: entry.timestamp,
                    });
                    break;
                }
            }
        }
    }

    result
}

/// Reconstruct active reservations from the journal on startup.
/// This is used when loading from persisted state — active_reservations are in-memory only,
/// so we need to reconstruct them from the journal.
pub fn reconstruct_active_reservations(ledger: &mut ResourceLedger) {
    // Collect all reservation IDs from journal
    let mut reserve_entries: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut completed: HashSet<String> = HashSet::new();

    for (idx, entry) in ledger.journal.iter().enumerate() {
        if entry.reference.starts_with("reserve:") {
            let rid = entry
                .reference
                .strip_prefix("reserve:")
                .unwrap_or("")
                .to_string();
            reserve_entries.insert(rid, idx);
        } else if entry.reference.starts_with("commit:")
            || entry.reference.starts_with("commit_with_amount:")
            || entry.reference.starts_with("rollback:")
        {
            // Extract the reservation ID after the prefix
            let rid = if let Some(r) = entry.reference.strip_prefix("commit:") {
                r.to_string()
            } else if let Some(r) = entry.reference.strip_prefix("commit_with_amount:") {
                r.to_string()
            } else if let Some(r) = entry.reference.strip_prefix("rollback:") {
                r.to_string()
            } else {
                continue;
            };
            completed.insert(rid);
        }
    }

    // For any reserve entry without a matching commit/rollback, reconstruct the Reservation
    for (rid, idx) in &reserve_entries {
        if !completed.contains(rid) {
            let entry = &ledger.journal[*idx];
            let token_id = entry.metadata.get("token_id").cloned().unwrap_or_default();
            let commodity = entry.metadata.get("commodity").cloned().unwrap_or_default();
            let amount_str = entry.metadata.get("amount").cloned().unwrap_or_default();
            let amount = amount_str
                .parse::<rust_decimal::Decimal>()
                .unwrap_or_default();
            let idempotency_key = entry
                .metadata
                .get("idempotency_key")
                .cloned()
                .unwrap_or_default();

            // If metadata is empty (e.g. the reserve entry doesn't store token_id etc
            // in metadata), attempt to reconstruct from the entry structure
            let (resolved_token_id, resolved_commodity, resolved_amount) =
                if token_id.is_empty() || commodity.is_empty() {
                    // Parse from the reserve journal entry's ledger entries
                    // The reserve entry has: DR token:{id}:reserved:{commodity} +amount
                    let mut t_id = String::new();
                    let mut comm = String::new();
                    let mut amt = rust_decimal::Decimal::ZERO;
                    for le in &entry.entries {
                        if le.amount.value > rust_decimal::Decimal::ZERO
                            && le.account.contains(":reserved:")
                        {
                            // Parse "token:{token_id}:reserved:{commodity}"
                            let parts: Vec<&str> = le.account.split(':').collect();
                            if parts.len() >= 4 {
                                t_id = parts[1].to_string();
                                comm = parts[3].to_string();
                            }
                            amt = le.amount.value;
                        }
                    }
                    (t_id, comm, amt)
                } else {
                    (token_id, commodity, amount)
                };

            let reservation = Reservation {
                id: ReservationId(rid.clone()),
                token_id: resolved_token_id,
                commodity: resolved_commodity,
                amount: resolved_amount,
                agent_id: entry.agent_id.clone(),
                created_at: entry.timestamp,
                idempotency_key,
                max_duration_secs: 120,
                batch_id: entry
                    .metadata
                    .get("batch_id")
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            };
            ledger
                .active_reservations
                .insert(reservation.id.clone(), reservation);
        }
    }
}
