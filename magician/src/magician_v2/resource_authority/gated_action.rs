//! Gated action type system: wraps `ExecutableAction` so spend-bearing actions
//! carry their gate metadata structurally.
//!
//! `MaybeGatedAction` is the return type of `CapabilityProvider::lower()` and
//! the type stored on `ExecutableStep.action`. Live spend is settled by
//! `spend_session::admit`, not by executing this wrapper.

use serde::{Deserialize, Serialize};

use crate::magician_v2::execution::actions::ExecutableAction;

use super::spend_gate::SpendGate;

/// A spend-bearing action: the inner `ExecutableAction` is private to ensure
/// callers cannot bypass the gate.
///
/// `Serialize` and `Deserialize` are implemented manually to prevent serde
/// from constructing a `SpendGatedAction` with a fabricated `action` field
/// (P2-19). The implementations mirror the derive behavior structurally.
#[derive(Debug, Clone)]
pub struct SpendGatedAction {
    action: ExecutableAction,
    pub gate: SpendGate,
}

// Manual Serialize: delegates to a helper struct that mirrors the field layout.
// This keeps MaybeGatedAction's derive working.
impl Serialize for SpendGatedAction {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct SpendGatedActionRef<'a> {
            action: &'a ExecutableAction,
            gate: &'a SpendGate,
        }
        SpendGatedActionRef {
            action: &self.action,
            gate: &self.gate,
        }
        .serialize(serializer)
    }
}

// Manual Deserialize: only allowed through SpendGatedAction::new() in production.
// This impl exists solely to satisfy MaybeGatedAction's derive requirement.
impl<'de> Deserialize<'de> for SpendGatedAction {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct SpendGatedActionOwned {
            action: ExecutableAction,
            gate: SpendGate,
        }
        let owned = SpendGatedActionOwned::deserialize(deserializer)?;
        Ok(Self {
            action: owned.action,
            gate: owned.gate,
        })
    }
}

impl SpendGatedAction {
    /// Create a new gated action.
    pub fn new(action: ExecutableAction, gate: SpendGate) -> Self {
        Self { action, gate }
    }

    /// Read-only access to the inner action (for logging, classification, etc.).
    pub fn inner_action(&self) -> &ExecutableAction {
        &self.action
    }

    /// Unwrap the inner action after spend metadata has been copied off.
    pub fn into_inner(self) -> ExecutableAction {
        self.action
    }
}

/// An action that may or may not require spend authorization.
///
/// - `Bare`: no spend gate — execute directly.
/// - `Gated`: carries a [`SpendGate`]; live writers settle it via
///   `spend_session::admit` before I/O.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "gated_type", rename_all = "snake_case")]
pub enum MaybeGatedAction {
    /// No spend gate required.
    Bare(ExecutableAction),
    /// Spend metadata for `spend_session::admit`; inner action is inspectable.
    Gated(SpendGatedAction),
}

impl MaybeGatedAction {
    /// Get a reference to the inner `ExecutableAction` regardless of gating.
    /// Used for logging, classification, observation policy, and pattern matching
    /// that needs to inspect the action type without bypassing the gate.
    pub fn inner_action(&self) -> &ExecutableAction {
        match self {
            MaybeGatedAction::Bare(action) => action,
            MaybeGatedAction::Gated(gated) => gated.inner_action(),
        }
    }

    /// Returns `true` if this action carries a spend gate.
    pub fn is_gated(&self) -> bool {
        matches!(self, MaybeGatedAction::Gated(_))
    }

    /// Returns `true` if the inner action is a browser action.
    pub fn is_browser(&self) -> bool {
        self.inner_action().is_browser()
    }

    /// Returns `true` if the inner action is a file action.
    pub fn is_file(&self) -> bool {
        self.inner_action().is_file()
    }

    /// Returns `true` if the inner action is a bash/shell action.
    pub fn is_bash(&self) -> bool {
        self.inner_action().is_bash()
    }

    /// Returns `true` if the inner action is an HTTP action.
    pub fn is_http(&self) -> bool {
        self.inner_action().is_http()
    }
}
