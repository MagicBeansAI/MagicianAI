//! The chat turn's hold for secrets it collected and has not yet handed on.
//!
//! A chat `need_user_input` owns no run of its own, so a sensitive answer has
//! nowhere to be vaulted when it arrives: the run that will use it — a
//! capability-pack sub-run — starts later in the same turn, when the model
//! calls the pack with the placeholder it was given. This hold bridges the two.
//! It is per session, in memory, zeroizing, bounded by a deadline, seeded into
//! every sub-run the turn starts (where `vault_flagged_resolved_inputs` moves
//! it into the run's scope), and dropped when the turn ends or is cancelled.
//! A later turn re-asks.

use std::collections::HashMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::magician_v2::execution::agentic::types::SeededSensitiveInput;
use crate::magician_v2::user_requests::SensitiveKind;

/// How long a collected secret waits for the pack call that uses it. The
/// same bound the request service's custody applies (`CUSTODY_HOLD_MAX_MS`).
pub const TURN_HOLD_MAX_MS: i64 = 15 * 60 * 1000;

struct Held {
    kind: SensitiveKind,
    value: Zeroizing<String>,
    deadline_ms: i64,
}

#[derive(Default)]
pub struct ChatTurnSecrets {
    held: Mutex<HashMap<String, Held>>,
}

impl std::fmt::Debug for ChatTurnSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<String> = self
            .held
            .lock()
            .map(|held| held.keys().cloned().collect())
            .unwrap_or_default();
        formatter
            .debug_struct("ChatTurnSecrets")
            .field("keys", &keys)
            .finish()
    }
}

impl ChatTurnSecrets {
    /// Hold `value` under `key` until `now_ms + TURN_HOLD_MAX_MS`. A later
    /// deposit under the same key replaces the earlier one.
    pub fn deposit(
        &self,
        key: impl Into<String>,
        kind: SensitiveKind,
        value: Zeroizing<String>,
        now_ms: i64,
    ) {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.insert(
            key.into(),
            Held {
                kind,
                value,
                deadline_ms: now_ms.saturating_add(TURN_HOLD_MAX_MS),
            },
        );
    }

    /// Everything still within its window, for seeding a sub-run. Expired
    /// entries are dropped on the way — and so is ONE-TIME material, which
    /// leaves the hold with the first sub-run that could spend it: seeding a
    /// second run with a copy gave the turn two runs each able to submit the
    /// same code once, which is exactly what one-time custody exists to
    /// prevent. A turn that needs another code asks for another code.
    pub fn seeds(&self, now_ms: i64) -> Vec<SeededSensitiveInput> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.retain(|_, entry| now_ms < entry.deadline_ms);
        let seeds: Vec<SeededSensitiveInput> = held
            .iter()
            .map(|(key, entry)| SeededSensitiveInput {
                key: key.clone(),
                kind: entry.kind,
                value: entry.value.clone(),
            })
            .collect();
        held.retain(|_, entry| entry.kind != SensitiveKind::Otp);
        seeds
    }

    /// The keys currently held, value-free — for the model's status.
    pub fn keys(&self) -> Vec<String> {
        self.held
            .lock()
            .map(|held| held.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.held.lock().map(|held| held.is_empty()).unwrap_or(true)
    }
}

/// The resolved-input key a chat answer is seeded under, and so the
/// placeholder the model uses: a form field keeps its id; a single value is
/// keyed by its kind.
pub fn seeded_key(kind: SensitiveKind, field: Option<&str>) -> String {
    match field {
        Some(field) if !field.is_empty() => field.to_string(),
        _ => match kind {
            SensitiveKind::LoginIdentifier => "login_identifier".to_string(),
            SensitiveKind::Password => "password".to_string(),
            SensitiveKind::Otp => "otp".to_string(),
            SensitiveKind::Other => "secret".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_is_bounded_keyed_and_value_free_in_debug() {
        const CANARY: &str = "turn-hold-canary-1";
        let hold = ChatTurnSecrets::default();
        assert!(hold.is_empty());
        hold.deposit(
            "password",
            SensitiveKind::Password,
            Zeroizing::new(CANARY.into()),
            1_000,
        );
        hold.deposit(
            "otp",
            SensitiveKind::Otp,
            Zeroizing::new("007123".into()),
            1_000,
        );
        assert!(!format!("{hold:?}").contains(CANARY));
        let mut keys = hold.keys();
        keys.sort();
        assert_eq!(keys, ["otp", "password"]);

        let mut seeds = hold.seeds(1_000 + TURN_HOLD_MAX_MS - 1);
        seeds.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[1].key, "password");
        assert_eq!(seeds[1].kind, SensitiveKind::Password);
        assert_eq!(seeds[1].value.as_str(), CANARY);
        assert!(!format!("{:?}", seeds[1]).contains(CANARY));

        // The code left with that seeding; the password is still held for the
        // turn's next pack call.
        let again = hold.seeds(1_000 + 1);
        assert_eq!(
            again.len(),
            1,
            "{:?}",
            again.iter().map(|s| &s.key).collect::<Vec<_>>()
        );
        assert_eq!(again[0].key, "password");
        assert_eq!(hold.keys(), ["password"]);

        assert!(
            hold.seeds(1_000 + TURN_HOLD_MAX_MS).is_empty(),
            "the window closes"
        );
        assert!(hold.is_empty(), "and expired material is gone, not kept");
    }

    #[test]
    fn seeded_keys_follow_the_field_or_the_kind() {
        assert_eq!(seeded_key(SensitiveKind::Password, Some("pw")), "pw");
        assert_eq!(seeded_key(SensitiveKind::Password, None), "password");
        assert_eq!(seeded_key(SensitiveKind::Otp, Some("")), "otp");
        assert_eq!(
            seeded_key(SensitiveKind::LoginIdentifier, None),
            "login_identifier"
        );
        assert_eq!(seeded_key(SensitiveKind::Other, None), "secret");
    }
}
