//! The owner's intervention policy (plan §8, P7.2): how hard Magician should
//! step in when an act conflicts with what the owner wants.
//!
//! # What it is
//!
//! An explicit, per-owner (`principal` + `workspace`) setting the owner
//! chooses on an owner-private surface. It has a global choice and optional
//! refinements for single intentions. Absent means the reviewed defaults. It
//! is never inferred: no reply, cancelled task or ignored nudge changes it.
//!
//! # A weaker global setting never waives a stronger instruction
//!
//! An explicit standing instruction ("check with me before ordering meat")
//! keeps its force when the owner picks a weaker global setting such as
//! `advisory`: [`OwnerPolicy::apply`] pins it to `confirm_before_exception`.
//! Only a refinement that names that one intention can change it — the owner
//! turning down one instruction on purpose, not a global switch doing it
//! silently.
//!
//! # Writes
//!
//! A write replaces the whole policy and carries the revision it was based
//! on. A write against an older revision is refused as stale, so two devices
//! cannot silently overwrite each other. The file is
//! `memory/users/preference_alignment_policy.json`, read and written under an
//! exclusive flock.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::contract::EffectivePolicy;
use super::intention::{Intention, IntentionKind, InterventionPolicy};
/// The reviewed-defaults policy version; an owner policy appends its revision.
pub use super::service::DEFAULT_POLICY_VERSION;
use crate::magician_v2::agents::storage::{AgentStorage, AgentStorageError};
use crate::magician_v2::agents::AgentMemoryResolver;

pub const ALIGNMENT_POLICY_FILE: &str = "preference_alignment_policy.json";
pub const ALIGNMENT_POLICY_SCHEMA_VERSION: &str = "alignment_policy.v1";
/// Most single-intention refinements one owner may keep.
pub const MAX_POLICY_REFINEMENTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerPolicy {
    pub schema_version: String,
    /// Bumped on every write. `0` is a policy that was never written.
    pub revision: u64,
    /// The owner's global choice. `None` is the reviewed defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<InterventionPolicy>,
    /// Refinements for single intentions, by intention id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub intentions: BTreeMap<String, InterventionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<i64>,
    /// The surface the owner changed it from (`web`, `ios`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_via: Option<String>,
}

impl Default for OwnerPolicy {
    fn default() -> Self {
        Self {
            schema_version: ALIGNMENT_POLICY_SCHEMA_VERSION.to_string(),
            revision: 0,
            global: None,
            intentions: BTreeMap::new(),
            updated_at_ms: None,
            updated_via: None,
        }
    }
}

impl OwnerPolicy {
    /// The policy the judge reads. The version names the revision, so an
    /// assessment records which owner choice it was made under.
    pub fn effective(&self) -> EffectivePolicy {
        EffectivePolicy {
            global: self.global,
            version: if self.revision == 0 {
                DEFAULT_POLICY_VERSION.to_string()
            } else {
                format!("{DEFAULT_POLICY_VERSION}+r{}", self.revision)
            },
        }
    }

    /// Put the owner's refinements on the intentions they name, and keep an
    /// explicit standing instruction from being weakened by the global
    /// choice. An intention's own policy, where its source has one, wins.
    pub fn apply(&self, intentions: &mut [Intention]) {
        for intention in intentions.iter_mut() {
            if intention.intervention_policy.is_some() {
                continue;
            }
            if let Some(refinement) = self.intentions.get(&intention.id) {
                intention.intervention_policy = Some(*refinement);
                continue;
            }
            let explicit_instruction = intention.kind == IntentionKind::StandingInstruction
                && intention.assertion_status.may_hold();
            let weaker_global = matches!(
                self.global,
                Some(InterventionPolicy::Advisory | InterventionPolicy::ChoiceForMaterial)
            );
            if explicit_instruction && weaker_global {
                intention.intervention_policy = Some(InterventionPolicy::ConfirmBeforeException);
            }
        }
    }
}

/// A whole-policy replacement, based on the revision the owner saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyChange {
    pub expected_revision: u64,
    #[serde(default)]
    pub global: Option<InterventionPolicy>,
    #[serde(default)]
    pub intentions: BTreeMap<String, InterventionPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyWrite {
    Applied(OwnerPolicy),
    /// The policy changed since the owner read it. Nothing was written; the
    /// current policy is returned so the surface can show it.
    Stale(OwnerPolicy),
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy scope unresolved: {0}")]
    Scope(String),
    #[error("policy lock failed: {0}")]
    Lock(String),
    #[error("policy unreadable: {0}")]
    Read(String),
    #[error("policy corrupt: {0}")]
    Corrupt(String),
    #[error("policy write failed: {0}")]
    Write(String),
    #[error("policy invalid: {0}")]
    Invalid(String),
}

pub fn alignment_policy_path(storage: &AgentStorage) -> PathBuf {
    storage.user_root().join(ALIGNMENT_POLICY_FILE)
}

fn storage_for(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<AgentStorage, PolicyError> {
    resolver
        .resolve_for_scope(principal, workspace)
        .map(|memory| memory.storage().clone())
        .map_err(|error| PolicyError::Scope(error.to_string()))
}

async fn read_file(storage: &AgentStorage, path: &PathBuf) -> Result<OwnerPolicy, PolicyError> {
    match storage.read_bytes(path).await {
        Ok(bytes) => {
            let policy: OwnerPolicy = serde_json::from_slice(&bytes)
                .map_err(|error| PolicyError::Corrupt(error.to_string()))?;
            if policy.schema_version != ALIGNMENT_POLICY_SCHEMA_VERSION {
                return Err(PolicyError::Corrupt(format!(
                    "unsupported schema version `{}`",
                    policy.schema_version
                )));
            }
            Ok(policy)
        },
        Err(AgentStorageError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
            Ok(OwnerPolicy::default())
        },
        Err(error) => Err(PolicyError::Read(error.to_string())),
    }
}

/// The owner's current policy. A missing file is the reviewed defaults.
/// The highest revision a replacement of an unreadable file may start from.
const MAX_REPLACEMENT_REVISION: u64 = 1 << 40;

pub async fn read_policy(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
) -> Result<OwnerPolicy, PolicyError> {
    let storage = storage_for(resolver, principal, workspace)?;
    let path = alignment_policy_path(&storage);
    read_file(&storage, &path).await
}

/// The policy as the history shows it: the global choice and the
/// per-intention refinements (ids only).
fn policy_summary(policy: &OwnerPolicy) -> String {
    serde_json::json!({"global": policy.global, "intentions": policy.intentions}).to_string()
}

/// Replace the owner's policy if it is still at `expected_revision`.
///
/// `replace_unreadable` lets the owner overwrite a policy file that cannot be
/// read, which would otherwise refuse every write and keep every check
/// unresolved: a fail-closed state must stay clearable by the owner.
pub async fn write_policy(
    resolver: &AgentMemoryResolver,
    principal: &str,
    workspace: &str,
    change: PolicyChange,
    replace_unreadable: bool,
    via: Option<&str>,
    now_ms: i64,
) -> Result<PolicyWrite, PolicyError> {
    if change.intentions.len() > MAX_POLICY_REFINEMENTS {
        return Err(PolicyError::Invalid(format!(
            "at most {MAX_POLICY_REFINEMENTS} intention refinements"
        )));
    }
    if change
        .intentions
        .keys()
        .any(|id| id.trim().is_empty() || id.len() > 256)
    {
        return Err(PolicyError::Invalid(
            "an intention id is empty or too long".to_string(),
        ));
    }
    let storage = storage_for(resolver, principal, workspace)?;
    let path = alignment_policy_path(&storage);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| PolicyError::Write(error.to_string()))?;
    }
    let _flock = AgentStorage::acquire_file_lock_exclusive(&path)
        .await
        .map_err(|error| PolicyError::Lock(error.to_string()))?;
    let current = match read_file(&storage, &path).await {
        Ok(current) => current,
        Err(PolicyError::Corrupt(_)) if replace_unreadable => OwnerPolicy {
            // Past any revision the unreadable file could have held, but
            // bounded: a client-chosen revision near `u64::MAX` would stall
            // every later compare-and-swap.
            revision: change.expected_revision.min(MAX_REPLACEMENT_REVISION),
            ..OwnerPolicy::default()
        },
        Err(error) => return Err(error),
    };
    if current.revision != change.expected_revision {
        return Ok(PolicyWrite::Stale(current));
    }
    let next = OwnerPolicy {
        schema_version: ALIGNMENT_POLICY_SCHEMA_VERSION.to_string(),
        revision: current.revision.saturating_add(1),
        global: change.global,
        intentions: change.intentions,
        updated_at_ms: Some(now_ms),
        updated_via: via.map(str::to_owned),
    };
    storage
        .write_json_atomic(&path, &next)
        .await
        .map_err(|error| PolicyError::Write(error.to_string()))?;
    // Journal the change after releasing the policy lock: the history takes
    // the knowledge lock, and no path holds both.
    drop(_flock);
    let mut event = super::history::event(
        "policy",
        super::history::HistoryOp::PolicyChanged,
        via.map(super::actor::WriteActor::surface)
            .unwrap_or_else(super::actor::WriteActor::unknown),
        now_ms,
    );
    event.revision_before = Some(format!("r{}", current.revision));
    event.revision_after = Some(format!("r{}", next.revision));
    event.wording_before = Some(policy_summary(&current));
    event.wording_after = Some(policy_summary(&next));
    if let Err(error) = super::history::append_event(&storage, event).await {
        tracing::warn!(error = %error, "preference history: policy change not journaled");
    }
    Ok(PolicyWrite::Applied(next))
}

#[cfg(test)]
mod tests {
    use super::super::intention::{
        AssertionStatus, IntentionLifecycle, IntentionSource, INTENTION_SCHEMA_VERSION,
    };
    use super::*;

    fn intention(id: &str, kind: IntentionKind, status: AssertionStatus) -> Intention {
        Intention {
            schema_version: INTENTION_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            revision: "r1".to_string(),
            source: IntentionSource::NativeTier {
                tier: "preferences".to_string(),
            },
            kind,
            assertion_status: status,
            statement: "x".to_string(),
            conditions: None,
            scope: None,
            lifecycle: IntentionLifecycle::Active,
            valid_until: None,
            evidence_refs: Vec::new(),
            intervention_policy: None,
        }
    }

    #[test]
    fn a_weaker_global_choice_never_waives_an_explicit_standing_instruction() {
        let policy = OwnerPolicy {
            revision: 3,
            global: Some(InterventionPolicy::Advisory),
            ..OwnerPolicy::default()
        };
        let mut intentions = vec![
            intention(
                "check-first",
                IntentionKind::StandingInstruction,
                AssertionStatus::DirectOwnerStatement,
            ),
            intention("taste", IntentionKind::Taste, AssertionStatus::DirectOwnerStatement),
            // An inferred "instruction" is not one the owner selected.
            intention(
                "guessed",
                IntentionKind::StandingInstruction,
                AssertionStatus::Inferred,
            ),
        ];
        policy.apply(&mut intentions);
        assert_eq!(
            intentions[0].intervention_policy,
            Some(InterventionPolicy::ConfirmBeforeException)
        );
        assert_eq!(intentions[1].intervention_policy, None, "the global applies");
        assert_eq!(intentions[2].intervention_policy, None);
        assert_eq!(policy.effective().global, Some(InterventionPolicy::Advisory));
        assert_eq!(policy.effective().version, "preference_alignment_policy.v1+r3");
    }

    #[test]
    fn a_refinement_naming_one_intention_is_the_owners_choice_for_it() {
        let mut policy = OwnerPolicy {
            revision: 1,
            global: Some(InterventionPolicy::Advisory),
            ..OwnerPolicy::default()
        };
        policy
            .intentions
            .insert("check-first".to_string(), InterventionPolicy::ChoiceForMaterial);
        let mut intentions = vec![intention(
            "check-first",
            IntentionKind::StandingInstruction,
            AssertionStatus::DirectOwnerStatement,
        )];
        policy.apply(&mut intentions);
        assert_eq!(
            intentions[0].intervention_policy,
            Some(InterventionPolicy::ChoiceForMaterial)
        );
    }

    #[test]
    fn defaults_change_nothing() {
        let policy = OwnerPolicy::default();
        let mut intentions = vec![intention(
            "check-first",
            IntentionKind::StandingInstruction,
            AssertionStatus::DirectOwnerStatement,
        )];
        policy.apply(&mut intentions);
        assert_eq!(intentions[0].intervention_policy, None);
        assert_eq!(policy.effective().version, DEFAULT_POLICY_VERSION);
    }

    #[tokio::test]
    async fn a_write_needs_the_revision_it_was_based_on() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let read = read_policy(&resolver, "owner", "ws").await.unwrap();
        assert_eq!(read, OwnerPolicy::default());

        let change = PolicyChange {
            expected_revision: 0,
            global: Some(InterventionPolicy::ChoiceForMaterial),
            intentions: BTreeMap::new(),
        };
        let PolicyWrite::Applied(written) = write_policy(
            &resolver,
            "owner",
            "ws",
            change.clone(),
            false,
            Some("web"),
            10,
        )
        .await
        .unwrap() else {
            panic!("first write applies");
        };
        assert_eq!(written.revision, 1);
        assert_eq!(written.updated_via.as_deref(), Some("web"));

        // A second device still holding revision 0 is refused.
        let stale = write_policy(&resolver, "owner", "ws", change, false, Some("ios"), 11)
            .await
            .unwrap();
        assert_eq!(stale, PolicyWrite::Stale(written.clone()));
        assert_eq!(read_policy(&resolver, "owner", "ws").await.unwrap(), written);
        // Another owner's scope is untouched.
        assert_eq!(
            read_policy(&resolver, "owner", "other").await.unwrap(),
            OwnerPolicy::default()
        );
    }

    #[tokio::test]
    async fn an_unreadable_policy_stays_clearable_by_the_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let resolver = AgentMemoryResolver::new(tmp.path());
        let storage = storage_for(&resolver, "owner", "ws").unwrap();
        let path = alignment_policy_path(&storage);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            read_policy(&resolver, "owner", "ws").await,
            Err(PolicyError::Corrupt(_))
        ));
        let change = PolicyChange {
            expected_revision: 0,
            global: None,
            intentions: BTreeMap::new(),
        };
        assert!(matches!(
            write_policy(&resolver, "owner", "ws", change.clone(), false, None, 1).await,
            Err(PolicyError::Corrupt(_))
        ));
        assert!(matches!(
            write_policy(&resolver, "owner", "ws", change, true, None, 1)
                .await
                .unwrap(),
            PolicyWrite::Applied(_)
        ));
        assert_eq!(read_policy(&resolver, "owner", "ws").await.unwrap().revision, 1);
    }
}
